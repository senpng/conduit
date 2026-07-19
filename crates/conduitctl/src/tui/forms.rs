//! Overlay form / wizard state for write operations.

use chrono::{Datelike, Utc};

use crate::dto::{
    CreateKeyBody, CreateProviderBody, CreateRouteBody, ProviderView, RouteTargetSpec, RouteView,
    SetSecretBody, UpdateProviderBody,
};

use super::input::InputField;

/// API-key (non-OAuth) provider kinds offered in the key form.
/// OAuth is a separate add path on the Providers tab.
pub const PROVIDER_KINDS: &[&str] = &["openai", "anthropic"];

/// Choices when pressing `a` on Providers.
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
        let mut i = self.selected as i32 + delta;
        if i < 0 {
            i = n - 1;
        }
        if i >= n {
            i = 0;
        }
        self.selected = i as usize;
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

    pub fn edit(p: &ProviderView) -> Self {
        let kind_idx = PROVIDER_KINDS
            .iter()
            .position(|k| *k == p.kind.as_str())
            .unwrap_or(0);
        // Kind is immutable on the API — only name + base_url are editable fields.
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
        match self.kind {
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
        // Edit form fields: [0]=name, [1]=base_url (kind is not editable).
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

#[derive(Debug, Clone)]
pub struct KeyForm {
    pub fields: Vec<InputField>,
    pub focus: usize,
    pub error: Option<String>,
}

impl KeyForm {
    pub fn create() -> Self {
        Self {
            fields: vec![
                InputField::new(""),
                InputField::new(""), // rpm optional
                InputField::new(""), // whitelist comma-separated
            ],
            focus: 0,
            error: None,
        }
    }

    pub fn labels() -> [&'static str; 3] {
        ["Name", "Rate limit RPM (optional)", "Model whitelist (comma-separated, empty=all)"]
    }

    pub fn to_body(&self) -> Result<CreateKeyBody, String> {
        let name = self.fields[0].value.trim();
        if name.is_empty() {
            return Err("name is required".into());
        }
        let rpm = {
            let s = self.fields[1].value.trim();
            if s.is_empty() {
                None
            } else {
                Some(
                    s.parse::<i64>()
                        .map_err(|_| format!("invalid rpm: {s}"))?,
                )
            }
        };
        let whitelist = {
            let s = self.fields[2].value.trim();
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
        };
        Ok(CreateKeyBody {
            name: name.to_string(),
            model_whitelist: whitelist,
            rate_limit_rpm: rpm,
        })
    }
}

#[derive(Debug, Clone)]
pub struct TargetDraft {
    /// Stable provider id (not index) so late-loaded provider lists keep selection.
    pub provider_id: String,
    pub model_id: InputField,
    pub overrides: InputField,
}

#[derive(Debug, Clone)]
pub struct RouteWizard {
    pub edit_id: Option<String>,
    /// 0 = alias/strategy, 1 = targets, 2 = review
    pub step: usize,
    pub match_alias: InputField,
    pub strategy_idx: usize,
    pub targets: Vec<TargetDraft>,
    pub target_focus: usize,
    /// 0 = model, 1 = overrides, when on step 1
    pub field_in_target: usize,
    pub providers: Vec<ProviderView>,
    pub error: Option<String>,
}

impl RouteWizard {
    pub fn create(providers: Vec<ProviderView>) -> Self {
        let mut w = Self {
            edit_id: None,
            step: 0,
            match_alias: InputField::new(""),
            strategy_idx: 0,
            targets: vec![],
            target_focus: 0,
            field_in_target: 0,
            providers,
            error: None,
        };
        w.add_target();
        w
    }

    pub fn edit(route: &RouteView, providers: Vec<ProviderView>) -> Self {
        let mut targets = Vec::new();
        if let Ok(arr) = serde_json::from_str::<Vec<serde_json::Value>>(&route.targets_json) {
            for t in arr {
                let pid = t
                    .get("provider_id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let model = t
                    .get("model_id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let overrides = t
                    .get("request_overrides")
                    .map(|v| {
                        if v.is_null() || v.as_object().is_some_and(|m| m.is_empty()) {
                            String::new()
                        } else {
                            v.to_string()
                        }
                    })
                    .unwrap_or_default();
                targets.push(TargetDraft {
                    provider_id: pid,
                    model_id: InputField::new(model),
                    overrides: InputField::new(overrides),
                });
            }
        }
        if targets.is_empty() {
            let default_pid = providers
                .first()
                .map(|p| p.id.clone())
                .unwrap_or_default();
            targets.push(TargetDraft {
                provider_id: default_pid,
                model_id: InputField::new(""),
                overrides: InputField::new(""),
            });
        }
        let strategy_idx = if route.strategy == "fallback" { 1 } else { 0 };
        Self {
            edit_id: Some(route.id.clone()),
            step: 0,
            match_alias: InputField::new(&route.match_alias),
            strategy_idx,
            targets,
            target_focus: 0,
            field_in_target: 0,
            providers,
            error: None,
        }
    }

    pub fn strategy(&self) -> &'static str {
        if self.strategy_idx == 0 {
            "fixed"
        } else {
            "fallback"
        }
    }

    pub fn cycle_strategy(&mut self) {
        self.strategy_idx = (self.strategy_idx + 1) % 2;
    }

    pub fn add_target(&mut self) {
        let default_pid = self
            .providers
            .first()
            .map(|p| p.id.clone())
            .unwrap_or_default();
        self.targets.push(TargetDraft {
            provider_id: default_pid,
            model_id: InputField::new(""),
            overrides: InputField::new(""),
        });
        self.target_focus = self.targets.len().saturating_sub(1);
    }

    pub fn remove_target(&mut self) {
        if self.targets.len() > 1 {
            self.targets
                .remove(self.target_focus.min(self.targets.len() - 1));
            self.target_focus = self.target_focus.min(self.targets.len().saturating_sub(1));
        }
    }

    pub fn cycle_provider(&mut self) {
        if self.providers.is_empty() {
            return;
        }
        if let Some(t) = self.targets.get_mut(self.target_focus) {
            let cur = self
                .providers
                .iter()
                .position(|p| p.id == t.provider_id)
                .unwrap_or(0);
            let next = (cur + 1) % self.providers.len();
            t.provider_id = self.providers[next].id.clone();
        }
    }

    /// Replace provider list without losing target `provider_id` bindings.
    pub fn set_providers(&mut self, providers: Vec<ProviderView>) {
        self.providers = providers;
        // If a target has empty provider_id, bind to first available.
        if let Some(first) = self.providers.first().map(|p| p.id.clone()) {
            for t in &mut self.targets {
                if t.provider_id.is_empty() {
                    t.provider_id = first.clone();
                }
            }
        }
    }

    pub fn provider_label(&self, provider_id: &str) -> String {
        self.providers
            .iter()
            .find(|p| p.id == provider_id)
            .map(|p| format!("{} ({})", p.name, p.kind))
            .unwrap_or_else(|| {
                if provider_id.is_empty() {
                    "(no provider)".into()
                } else {
                    format!("{provider_id} (missing)")
                }
            })
    }

    pub fn to_body(&self) -> Result<CreateRouteBody, String> {
        let alias = self.match_alias.value.trim();
        if alias.is_empty() {
            return Err("match_alias is required".into());
        }
        if self.providers.is_empty() {
            return Err("no providers available — add a provider first".into());
        }
        let mut specs = Vec::new();
        for (i, t) in self.targets.iter().enumerate() {
            let p = self
                .providers
                .iter()
                .find(|p| p.id == t.provider_id)
                .ok_or_else(|| {
                    format!(
                        "target {i}: provider {} not found — Ctrl-k to pick one",
                        t.provider_id
                    )
                })?;
            let model = t.model_id.value.trim();
            if model.is_empty() {
                return Err(format!("target {i}: model_id is required"));
            }
            let overrides = {
                let s = t.overrides.value.trim();
                if s.is_empty() {
                    serde_json::Map::new()
                } else {
                    let v: serde_json::Value = serde_json::from_str(s)
                        .map_err(|e| format!("target {i}: invalid overrides JSON: {e}"))?;
                    v.as_object()
                        .cloned()
                        .ok_or_else(|| format!("target {i}: overrides must be a JSON object"))?
                }
            };
            specs.push(RouteTargetSpec {
                provider_id: p.id.clone(),
                model_id: model.to_string(),
                upstream_key_id: p.id.clone(),
                provider_kind: p.kind.clone(),
                base_url: Some(p.base_url.clone()),
                request_overrides: overrides,
                pool_id: None,
                pool_kind: None,
            });
        }
        let targets = serde_json::to_value(&specs).map_err(|e| e.to_string())?;
        Ok(CreateRouteBody {
            match_alias: alias.to_string(),
            strategy: self.strategy().to_string(),
            targets,
            retry_policy: None,
        })
    }
}

#[derive(Debug, Clone)]
pub struct OauthFlow {
    /// 0 = claude, 1 = codex, 2 = grok
    pub kind_idx: usize,
    pub name: InputField,
    pub provider_id: InputField,
    pub focus: usize,
    pub pending_session_id: Option<String>,
    pub session_status: Option<String>,
    pub auth_url: Option<String>,
    pub user_code: Option<String>,
    pub verification_uri: Option<String>,
    pub result_message: Option<String>,
    pub error: Option<String>,
    pub poll_ticks: u32,
}

impl OauthFlow {
    pub fn new() -> Self {
        Self {
            kind_idx: 0,
            name: InputField::new(""),
            provider_id: InputField::new(""),
            focus: 0,
            pending_session_id: None,
            session_status: None,
            auth_url: None,
            user_code: None,
            verification_uri: None,
            result_message: None,
            error: None,
            poll_ticks: 0,
        }
    }

    /// Start a new OAuth provider (from Providers → add).
    pub fn start_new(kind_idx: usize, name: &str) -> Self {
        let mut f = Self::new();
        f.kind_idx = kind_idx.min(2);
        if !name.is_empty() {
            f.name = InputField::new(name);
        }
        f.focus = 1; // name field first; kind is fixed from chooser
        f
    }

    /// Re-authenticate an existing OAuth provider row.
    pub fn reauth(p: &ProviderView) -> Self {
        let mut f = Self::new();
        f.provider_id = InputField::new(&p.id);
        f.name = InputField::new(&p.name);
        f.kind_idx = if p.kind.contains("claude") {
            0
        } else if p.kind.contains("codex") {
            1
        } else if p.kind.contains("grok") || p.kind.contains("xai") {
            2
        } else {
            0
        };
        f.focus = 0;
        f
    }

    pub fn kind(&self) -> &'static str {
        ["claude", "codex", "grok"][self.kind_idx.min(2)]
    }

    pub fn cycle_kind(&mut self) {
        // Kind is usually fixed by the add chooser; still allow cycle when re-authing.
        self.kind_idx = (self.kind_idx + 1) % 3;
    }
}

#[derive(Debug, Clone)]
pub struct PricingOverrideForm {
    pub fields: Vec<InputField>,
    pub focus: usize,
    pub error: Option<String>,
    /// When editing, original keys (for delete-before-rename if we ever rename).
    pub editing: bool,
}

impl PricingOverrideForm {
    /// Field order: provider_kind, model_id, input_per_mtok, output_per_mtok,
    /// cache_read (optional), cache_write (optional).
    pub fn create() -> Self {
        Self {
            fields: vec![
                InputField::new("openai"),
                InputField::new(""),
                InputField::new("1.0"),
                InputField::new("4.0"),
                InputField::new(""),
                InputField::new(""),
                InputField::new(""), // reasoning optional
            ],
            focus: 0,
            error: None,
            editing: false,
        }
    }

    pub fn edit(p: &crate::dto::PricingView) -> Self {
        let mut f = Self::from_row(p);
        f.editing = true;
        f
    }

    /// Prefill from a merged-table (or override) row to quickly create an override.
    /// Focus lands on input price so the operator can tweak and save.
    pub fn from_row(p: &crate::dto::PricingView) -> Self {
        Self {
            fields: vec![
                InputField::new(&p.provider_kind),
                InputField::new(&p.model_id),
                InputField::new(format_rate(p.input_per_mtok)),
                InputField::new(format_rate(p.output_per_mtok)),
                InputField::new(
                    p.cache_read_per_mtok
                        .map(format_rate)
                        .unwrap_or_default(),
                ),
                InputField::new(
                    p.cache_write_per_mtok
                        .map(format_rate)
                        .unwrap_or_default(),
                ),
                InputField::new(
                    p.reasoning_per_mtok
                        .map(format_rate)
                        .unwrap_or_default(),
                ),
            ],
            focus: 2, // Input $/MTok
            error: None,
            editing: false,
        }
    }

    pub fn labels() -> [&'static str; 7] {
        [
            "Provider kind",
            "Model id",
            "Input $/MTok",
            "Output $/MTok",
            "Cache read $/MTok (optional)",
            "Cache write $/MTok (optional)",
            "Reasoning $/MTok (optional)",
        ]
    }

    pub fn to_body(&self) -> Result<crate::dto::UpsertPricingOverrideBody, String> {
        let provider_kind = self.fields[0].value.trim();
        let model_id = self.fields[1].value.trim();
        if provider_kind.is_empty() || model_id.is_empty() {
            return Err("provider_kind and model_id are required".into());
        }
        let input_per_mtok: f64 = self.fields[2]
            .value
            .trim()
            .parse()
            .map_err(|_| "invalid input_per_mtok".to_string())?;
        let output_per_mtok: f64 = self.fields[3]
            .value
            .trim()
            .parse()
            .map_err(|_| "invalid output_per_mtok".to_string())?;
        if !input_per_mtok.is_finite()
            || !output_per_mtok.is_finite()
            || input_per_mtok < 0.0
            || output_per_mtok < 0.0
        {
            return Err("prices must be finite and non-negative".into());
        }
        if input_per_mtok == 0.0 && output_per_mtok == 0.0 {
            return Err("at least one of input/output must be positive".into());
        }
        let parse_opt = |s: &str| -> Result<Option<f64>, String> {
            let t = s.trim();
            if t.is_empty() {
                return Ok(None);
            }
            let v: f64 = t.parse().map_err(|_| format!("invalid number: {t}"))?;
            if !v.is_finite() || v < 0.0 {
                return Err(format!("invalid number: {t}"));
            }
            Ok(Some(v))
        };
        Ok(crate::dto::UpsertPricingOverrideBody {
            provider_kind: provider_kind.to_string(),
            model_id: model_id.to_string(),
            input_per_mtok,
            output_per_mtok,
            cache_read_per_mtok: parse_opt(&self.fields[4].value)?,
            cache_write_per_mtok: parse_opt(&self.fields[5].value)?,
            reasoning_per_mtok: self
                .fields
                .get(6)
                .map(|f| parse_opt(&f.value))
                .transpose()?
                .flatten(),
            effective_from: None,
        })
    }
}

fn format_rate(v: f64) -> String {
    // Keep enough precision for sub-cent MTok rates without noisy trailing zeros.
    let s = format!("{v:.6}");
    s.trim_end_matches('0').trim_end_matches('.').to_string()
}

#[derive(Debug, Clone)]
pub enum ConfirmAction {
    DeleteProvider { id: String, name: String },
    DeleteRoute { id: String, alias: String },
    DeleteKey { id: String, name: String },
    SetProviderSecret { id: String, name: String },
    DeletePricingOverride {
        provider_kind: String,
        model_id: String,
    },
}

pub fn current_period() -> String {
    let now = Utc::now();
    format!("{:04}-{:02}", now.year(), now.month())
}

pub fn shift_period(period: &str, delta: i32) -> String {
    let parts: Vec<_> = period.split('-').collect();
    if parts.len() != 2 {
        return current_period();
    }
    let y: i32 = parts[0].parse().unwrap_or(Utc::now().year());
    let m: u32 = parts[1].parse().unwrap_or(Utc::now().month());
    let mut month = m as i32 + delta;
    let mut year = y;
    while month < 1 {
        month += 12;
        year -= 1;
    }
    while month > 12 {
        month -= 12;
        year += 1;
    }
    format!("{:04}-{:02}", year, month)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn route_wizard_builds_targets() {
        let providers = vec![ProviderView {
            id: "p1".into(),
            name: "oai".into(),
            kind: "openai".into(),
            base_url: "https://api.openai.com/v1".into(),
            upstream_key_ref: String::new(),
            created_at: String::new(),
            updated_at: String::new(),
        }];
        let mut w = RouteWizard::create(providers);
        w.match_alias.value = "gpt-4o".into();
        w.targets[0].model_id.value = "gpt-4o".into();
        let body = w.to_body().unwrap();
        assert_eq!(body.match_alias, "gpt-4o");
        assert_eq!(body.strategy, "fixed");
        let arr = body.targets.as_array().unwrap();
        assert_eq!(arr[0]["provider_id"], "p1");
        assert_eq!(arr[0]["upstream_key_id"], "p1");
    }

    #[test]
    fn provider_edit_body_uses_name_and_base_url_only() {
        let p = ProviderView {
            id: "id1".into(),
            name: "old".into(),
            kind: "openai".into(),
            base_url: "https://old.example".into(),
            upstream_key_ref: String::new(),
            created_at: String::new(),
            updated_at: String::new(),
        };
        let mut f = ProviderForm::edit(&p);
        assert_eq!(f.fields.len(), 2, "edit form must not include kind field");
        f.fields[0].value = "new".into();
        f.fields[1].value = "https://new.example".into();
        let body = f.to_update_body().unwrap();
        assert_eq!(body.name.as_deref(), Some("new"));
        assert_eq!(body.base_url.as_deref(), Some("https://new.example"));
    }

    #[test]
    fn route_edit_preserves_provider_id_when_list_loads_later() {
        let route = RouteView {
            id: "r1".into(),
            match_alias: "gpt-4o".into(),
            strategy: "fixed".into(),
            targets_json: r#"[{"provider_id":"p2","model_id":"m","upstream_key_id":"p2","provider_kind":"openai"}]"#.into(),
            retry_policy_json: String::new(),
            enabled: true,
            created_at: String::new(),
            updated_at: String::new(),
        };
        let mut w = RouteWizard::edit(&route, vec![]);
        assert_eq!(w.targets[0].provider_id, "p2");
        w.set_providers(vec![
            ProviderView {
                id: "p1".into(),
                name: "a".into(),
                kind: "openai".into(),
                base_url: "https://a".into(),
                upstream_key_ref: String::new(),
                created_at: String::new(),
                updated_at: String::new(),
            },
            ProviderView {
                id: "p2".into(),
                name: "b".into(),
                kind: "openai".into(),
                base_url: "https://b".into(),
                upstream_key_ref: String::new(),
                created_at: String::new(),
                updated_at: String::new(),
            },
        ]);
        assert_eq!(w.targets[0].provider_id, "p2");
        w.targets[0].model_id.value = "m".into();
        let body = w.to_body().unwrap();
        assert_eq!(body.targets[0]["provider_id"], "p2");
    }

    #[test]
    fn shift_period_wraps_year() {
        assert_eq!(shift_period("2026-01", -1), "2025-12");
        assert_eq!(shift_period("2025-12", 1), "2026-01");
    }
}
