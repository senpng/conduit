//! Provider add chooser + create/edit/set-secret forms.

use crate::dto::{CreateProviderBody, ProviderView, SetSecretBody, UpdateProviderBody};

use super::super::input::InputField;

/// API-key (non-OAuth) provider kinds offered in the key form.
/// OAuth is a separate add path on the Providers tab.
pub const PROVIDER_KINDS: &[&str] = &["openai", "anthropic"];

pub const PROVIDER_ADD_OPTIONS: &[&str] = &[
    "API key   — openai / anthropic (+ paste secret)",
    "OAuth     — Claude subscription login",
    "OAuth     — Codex subscription login",
    "OAuth     — Grok subscription login",
];

#[derive(Debug, Clone)]
pub struct ProviderAddChooser {
    pub selected: usize,
}

impl ProviderAddChooser {
    pub fn new() -> Self {
        Self { selected: 0 }
    }

    pub fn len(&self) -> usize {
        PROVIDER_ADD_OPTIONS.len()
    }

    pub fn move_sel(&mut self, delta: i32) {
        let n = self.len() as i32;
        if n == 0 {
            return;
        }
        // Clamp at ends — no circular wrap.
        self.selected = (self.selected as i32 + delta).clamp(0, n - 1) as usize;
    }
}

#[derive(Debug, Clone)]
pub enum ProviderFormKind {
    Create,
    Edit { id: String },
    SetSecret { id: String, name: String },
}

#[derive(Debug, Clone)]
pub struct ProviderForm {
    pub kind: ProviderFormKind,
    pub fields: Vec<InputField>,
    pub focus: usize,
    pub kind_idx: usize,
    /// Display-only kind for edit mode (API cannot change kind).
    pub kind_display: Option<String>,
    pub error: Option<String>,
}

impl ProviderForm {
    pub fn create() -> Self {
        Self {
            kind: ProviderFormKind::Create,
            fields: vec![
                InputField::new(""),
                InputField::new("openai"),
                InputField::new("https://api.openai.com/v1"),
                InputField::new("").password(),
            ],
            focus: 0,
            kind_idx: 0,
            kind_display: None,
            error: None,
        }
    }

    /// Edit name / base_url only.
    pub fn edit(p: &ProviderView) -> Self {
        let kind_idx = PROVIDER_KINDS
            .iter()
            .position(|k| *k == p.kind.as_str())
            .unwrap_or(0);
        Self {
            kind: ProviderFormKind::Edit { id: p.id.clone() },
            fields: vec![InputField::new(&p.name), InputField::new(&p.base_url)],
            focus: 0,
            kind_idx,
            kind_display: Some(p.kind.clone()),
            error: None,
        }
    }

    pub fn set_secret(id: &str, name: &str) -> Self {
        Self {
            kind: ProviderFormKind::SetSecret {
                id: id.to_string(),
                name: name.to_string(),
            },
            fields: vec![InputField::new("").password()],
            focus: 0,
            kind_idx: 0,
            kind_display: None,
            error: None,
        }
    }

    pub fn labels(&self) -> Vec<&'static str> {
        match &self.kind {
            ProviderFormKind::Create => vec!["Name", "Kind", "Base URL", "API Key (optional)"],
            ProviderFormKind::Edit { .. } => vec!["Name", "Base URL"],
            ProviderFormKind::SetSecret { .. } => vec!["API Key"],
        }
    }

    pub fn title(&self) -> String {
        match &self.kind {
            ProviderFormKind::Create => "Add Provider".into(),
            ProviderFormKind::Edit { id } => {
                let kind = self.kind_display.as_deref().unwrap_or("?");
                format!("Edit Provider {id} ({kind})")
            }
            ProviderFormKind::SetSecret { name, .. } => format!("Set Secret — {name}"),
        }
    }

    pub fn cycle_kind(&mut self) {
        if matches!(self.kind, ProviderFormKind::Create) {
            self.kind_idx = (self.kind_idx + 1) % PROVIDER_KINDS.len();
            self.fields[1].value = PROVIDER_KINDS[self.kind_idx].to_string();
            self.fields[1].cursor = self.fields[1].value.chars().count();
            let url = match PROVIDER_KINDS[self.kind_idx] {
                "openai" => "https://api.openai.com/v1",
                "anthropic" => "https://api.anthropic.com",
                _ => "",
            };
            if !url.is_empty() {
                self.fields[2].value = url.to_string();
                self.fields[2].cursor = url.chars().count();
            }
        }
    }

    pub fn is_oauth_kind_label(kind: &str) -> bool {
        kind.contains("oauth") || kind == "claude" || kind == "codex" || kind == "grok"
    }

    pub fn to_create_body(&self) -> Result<CreateProviderBody, String> {
        let name = self.fields[0].value.trim();
        let kind = self.fields[1].value.trim();
        let base_url = self.fields[2].value.trim();
        if name.is_empty() || kind.is_empty() || base_url.is_empty() {
            return Err("name, kind, and base_url are required".into());
        }
        let mut body = CreateProviderBody::new(name, kind, base_url);
        let key = self.fields[3].value.trim();
        if !key.is_empty() {
            body = body.with_api_key(key);
        }
        Ok(body)
    }

    pub fn to_update_body(&self) -> Result<UpdateProviderBody, String> {
        let name = self
            .fields
            .first()
            .map(|f| f.value.trim())
            .unwrap_or("");
        let base_url = self
            .fields
            .get(1)
            .map(|f| f.value.trim())
            .unwrap_or("");
        if name.is_empty() || base_url.is_empty() {
            return Err("name and base_url are required".into());
        }
        Ok(UpdateProviderBody {
            name: Some(name.to_string()),
            base_url: Some(base_url.to_string()),
        })
    }

    pub fn to_secret_body(&self) -> Result<SetSecretBody, String> {
        let api_key = self.fields[0].value.trim();
        if api_key.is_empty() {
            return Err("api_key is required".into());
        }
        Ok(SetSecretBody {
            api_key: api_key.to_string(),
        })
    }
}


