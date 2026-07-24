//! Downstream key create/edit forms.

use crate::dto::{CreateKeyBody, KeyView, UpdateKeyBody};

use super::super::input::InputField;

#[derive(Debug, Clone)]
pub struct KeyForm {
    /// `None` = create; `Some(id)` = update existing key metadata.
    pub edit_id: Option<String>,
    pub fields: Vec<InputField>,
    pub focus: usize,
    pub error: Option<String>,
}

impl KeyForm {
    pub fn create() -> Self {
        Self {
            edit_id: None,
            fields: vec![
                InputField::new(""),
                InputField::new(""), // rpm optional
                InputField::new(""), // whitelist comma-separated
            ],
            focus: 0,
            error: None,
        }
    }

    pub fn edit(k: &KeyView) -> Self {
        let rpm = k
            .rate_limit_rpm
            .map(|r| r.to_string())
            .unwrap_or_default();
        let whitelist = whitelist_to_csv(&k.model_whitelist);
        let enabled = if k.enabled { "true" } else { "false" };
        Self {
            edit_id: Some(k.id.clone()),
            fields: vec![
                InputField::new(&k.name),
                InputField::new(rpm),
                InputField::new(whitelist),
                InputField::new(enabled),
            ],
            focus: 0,
            error: None,
        }
    }

    pub fn is_edit(&self) -> bool {
        self.edit_id.is_some()
    }

    pub fn title(&self) -> String {
        match &self.edit_id {
            Some(id) => format!("Edit key {id}"),
            None => "Create downstream key".into(),
        }
    }

    pub fn labels(&self) -> Vec<&'static str> {
        if self.is_edit() {
            vec![
                "Name",
                "Rate limit RPM (empty=unlimited)",
                "Model whitelist (comma-separated, empty=all)",
                "Enabled (true/false · Ctrl-k toggle)",
            ]
        } else {
            vec![
                "Name",
                "Rate limit RPM (optional)",
                "Model whitelist (comma-separated, empty=all)",
            ]
        }
    }

    /// Toggle enabled field on edit forms (Ctrl-k).
    pub fn cycle_enabled(&mut self) {
        if !self.is_edit() {
            return;
        }
        if let Some(f) = self.fields.get_mut(3) {
            let on = parse_bool_loose(&f.value).unwrap_or(true);
            let next = if on { "false" } else { "true" };
            f.value = next.to_string();
            f.cursor = next.chars().count();
        }
    }

    fn parse_rpm(s: &str) -> Result<Option<i64>, String> {
        let s = s.trim();
        if s.is_empty() {
            return Ok(None);
        }
        let v = s
            .parse::<i64>()
            .map_err(|_| format!("invalid rpm: {s}"))?;
        // Server stores this and later casts i64 → u32, so a negative
        // value wraps to a huge limit (rate-limiting disabled) and 0
        // rejects every request. Require a positive requests/minute.
        if v < 1 {
            return Err("rpm must be a positive number".into());
        }
        Ok(Some(v))
    }

    fn parse_whitelist(s: &str) -> Option<Vec<String>> {
        let s = s.trim();
        if s.is_empty() {
            None
        } else {
            Some(
                s.split(',')
                    .map(|x| x.trim().to_string())
                    .filter(|x| !x.is_empty())
                    .collect::<Vec<_>>(),
            )
        }
    }

    pub fn to_create_body(&self) -> Result<CreateKeyBody, String> {
        let name = self.fields[0].value.trim();
        if name.is_empty() {
            return Err("name is required".into());
        }
        Ok(CreateKeyBody {
            name: name.to_string(),
            model_whitelist: Self::parse_whitelist(&self.fields[2].value),
            rate_limit_rpm: Self::parse_rpm(&self.fields[1].value)?,
        })
    }

    /// Full-field update: name + whitelist always sent; empty rpm → unlimited;
    /// enabled from the 4th field.
    pub fn to_update_body(&self) -> Result<UpdateKeyBody, String> {
        let name = self.fields[0].value.trim();
        if name.is_empty() {
            return Err("name is required".into());
        }
        let enabled = {
            let s = self
                .fields
                .get(3)
                .map(|f| f.value.as_str())
                .unwrap_or("true");
            parse_bool_loose(s).ok_or_else(|| {
                format!("enabled must be true/false (got {s:?})")
            })?
        };
        // Always send whitelist (empty vec = allow all) so clears work.
        let whitelist = Self::parse_whitelist(&self.fields[2].value).unwrap_or_default();
        Ok(UpdateKeyBody {
            name: Some(name.to_string()),
            model_whitelist: Some(whitelist),
            rate_limit_rpm: Self::parse_rpm(&self.fields[1].value)?,
            enabled: Some(enabled),
        })
    }
}

fn whitelist_to_csv(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::Array(arr) => arr
            .iter()
            .filter_map(|x| x.as_str())
            .collect::<Vec<_>>()
            .join(", "),
        serde_json::Value::String(s) => s.clone(),
        _ => String::new(),
    }
}

fn parse_bool_loose(s: &str) -> Option<bool> {
    match s.trim().to_ascii_lowercase().as_str() {
        "true" | "t" | "yes" | "y" | "1" | "on" | "enabled" => Some(true),
        "false" | "f" | "no" | "n" | "0" | "off" | "disabled" => Some(false),
        _ => None,
    }
}
