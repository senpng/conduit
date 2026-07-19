//! Overlay form / wizard state for write operations.

use chrono::{Datelike, Local};

use crate::dto::{
    CreateKeyBody, CreateProviderBody, CreateRouteBody, KeyView, ProviderView, RouteTargetSpec,
    RouteView, SetSecretBody, UpdateKeyBody, UpdateProviderBody,
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

/// How a route target resolves upstream provider(s).
///
/// - [`Provider`](Self::Provider): fixed single account
/// - [`PoolKind`](Self::PoolKind): expand to all providers of that kind at
///   decision time (multi-account pool; round_robin/fill_first + session affinity)
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TargetBinding {
    Provider { id: String },
    PoolKind { kind: String },
}

impl TargetBinding {
    pub fn is_pool(&self) -> bool {
        matches!(self, Self::PoolKind { .. })
    }

    /// Parse binding from a stored target JSON object.
    pub fn from_target_json(t: &serde_json::Value) -> Self {
        let pool_kind = t
            .get("pool_kind")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty());
        let pool_id = t
            .get("pool_id")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty());
        if let Some(kind) = pool_kind {
            return Self::PoolKind {
                kind: kind.to_string(),
            };
        }
        if let Some(pid) = pool_id {
            // Auto kind-pool uses pool_id == kind name when no named pool exists.
            return Self::PoolKind {
                kind: pid.to_string(),
            };
        }
        let id = t
            .get("provider_id")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        Self::Provider { id }
    }
}

#[derive(Debug, Clone)]
pub struct TargetDraft {
    pub binding: TargetBinding,
    pub model_id: InputField,
    pub overrides: InputField,
}

#[derive(Debug, Clone)]
pub struct RouteWizard {
    pub edit_id: Option<String>,
    /// 0 = alias/strategy, 1 = targets, 2 = review
    pub step: usize,
    pub match_alias: InputField,
    /// 0 = fixed, 1 = fallback, 2 = weighted
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
                    binding: TargetBinding::from_target_json(&t),
                    model_id: InputField::new(model),
                    overrides: InputField::new(overrides),
                });
            }
        }
        if targets.is_empty() {
            targets.push(TargetDraft {
                binding: default_binding(&providers),
                model_id: InputField::new(""),
                overrides: InputField::new(""),
            });
        }
        // When any target is a pool, strategy stores pool schedule mode.
        let has_pool = targets.iter().any(|t| t.binding.is_pool());
        let strategy_idx = if has_pool {
            match route.strategy.as_str() {
                "fill_first" | "fill-first" | "fillfirst" => 1,
                _ => 0, // round_robin
            }
        } else {
            match route.strategy.as_str() {
                "fallback" => 1,
                "weighted" | "weight" | "lb" => 2,
                _ => 0,
            }
        };
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

    /// True when any target is a kind/named pool (multi-account).
    pub fn has_pool_target(&self) -> bool {
        self.targets.iter().any(|t| t.binding.is_pool())
    }

    pub fn strategy(&self) -> &'static str {
        if self.has_pool_target() {
            return match self.strategy_idx {
                1 => "fill_first",
                _ => "round_robin",
            };
        }
        match self.strategy_idx {
            1 => "fallback",
            2 => "weighted",
            _ => "fixed",
        }
    }

    pub fn strategy_hint(&self) -> &'static str {
        if self.has_pool_target() {
            return match self.strategy_idx {
                1 => "pool: fill-first · session affinity",
                _ => "pool: round-robin · session affinity",
            };
        }
        match self.strategy_idx {
            1 => "ordered failover · session affinity",
            2 => "weighted LB · session affinity",
            _ => "always first target",
        }
    }

    pub fn cycle_strategy(&mut self) {
        let n = if self.has_pool_target() { 2 } else { 3 };
        self.strategy_idx = (self.strategy_idx + 1) % n;
    }

    pub fn add_target(&mut self) {
        self.targets.push(TargetDraft {
            binding: default_binding(&self.providers),
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

    /// Cycle binding: each provider (single) then each distinct kind as a pool.
    pub fn cycle_provider(&mut self) {
        let opts = self.binding_options();
        if opts.is_empty() {
            return;
        }
        if let Some(t) = self.targets.get_mut(self.target_focus) {
            let cur = opts
                .iter()
                .position(|b| b == &t.binding)
                .unwrap_or(usize::MAX);
            let next = if cur == usize::MAX {
                0
            } else {
                (cur + 1) % opts.len()
            };
            t.binding = opts[next].clone();
        }
    }

    /// Options for Ctrl-k: singles first, then kind-pools (multi-account ready).
    pub fn binding_options(&self) -> Vec<TargetBinding> {
        let mut opts: Vec<TargetBinding> = self
            .providers
            .iter()
            .map(|p| TargetBinding::Provider {
                id: p.id.clone(),
            })
            .collect();
        let mut kinds: Vec<String> = self.providers.iter().map(|p| p.kind.clone()).collect();
        kinds.sort();
        kinds.dedup();
        for kind in kinds {
            opts.push(TargetBinding::PoolKind { kind });
        }
        opts
    }

    /// Replace provider list without losing target bindings.
    pub fn set_providers(&mut self, providers: Vec<ProviderView>) {
        self.providers = providers;
        let first = self.providers.first().map(|p| p.id.clone());
        for t in &mut self.targets {
            if let TargetBinding::Provider { id } = &mut t.binding {
                if id.is_empty() {
                    if let Some(ref f) = first {
                        *id = f.clone();
                    }
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

    pub fn kind_member_count(&self, kind: &str) -> usize {
        self.providers
            .iter()
            .filter(|p| p.kind.eq_ignore_ascii_case(kind))
            .count()
    }

    /// Human label for the focused (or any) target binding.
    pub fn binding_label(&self, binding: &TargetBinding) -> String {
        match binding {
            TargetBinding::Provider { id } => {
                format!("single · {}", self.provider_label(id))
            }
            TargetBinding::PoolKind { kind } => {
                let n = self.kind_member_count(kind);
                let accounts = if n == 1 { "account" } else { "accounts" };
                format!("pool · {kind} ({n} {accounts})")
            }
        }
    }

    pub fn to_body(&self) -> Result<CreateRouteBody, String> {
        let alias = self.match_alias.value.trim();
        if alias.is_empty() {
            return Err("match_alias is required".into());
        }
        if self.providers.is_empty() {
            return Err("no providers available — add a provider first".into());
        }
        if self.targets.is_empty() {
            return Err("at least one target is required".into());
        }
        let mut specs = Vec::new();
        for (i, t) in self.targets.iter().enumerate() {
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
            let spec = match &t.binding {
                TargetBinding::Provider { id } => {
                    let p = self.providers.iter().find(|p| p.id == *id).ok_or_else(|| {
                        format!(
                            "target {i}: provider {id} not found — Ctrl-k to pick one or a pool"
                        )
                    })?;
                    RouteTargetSpec {
                        provider_id: p.id.clone(),
                        model_id: model.to_string(),
                        provider_kind: p.kind.clone(),
                        base_url: Some(p.base_url.clone()),
                        request_overrides: overrides,
                        pool_id: None,
                        pool_kind: None,
                    }
                }
                TargetBinding::PoolKind { kind } => {
                    let n = self.kind_member_count(kind);
                    if n == 0 {
                        return Err(format!(
                            "target {i}: pool kind '{kind}' has no providers — Ctrl-k to pick"
                        ));
                    }
                    RouteTargetSpec {
                        // Empty provider_id: membership expands from catalog at route time.
                        provider_id: String::new(),
                        model_id: model.to_string(),
                        provider_kind: kind.clone(),
                        base_url: None,
                        request_overrides: overrides,
                        pool_id: None,
                        pool_kind: Some(kind.clone()),
                    }
                }
            };
            specs.push(spec);
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

fn default_binding(providers: &[ProviderView]) -> TargetBinding {
    providers
        .first()
        .map(|p| TargetBinding::Provider {
            id: p.id.clone(),
        })
        .unwrap_or(TargetBinding::Provider {
            id: String::new(),
        })
}

/// Compact one-line summary of route targets for the Routes list column.
pub fn summarize_route_targets(targets_json: &str, providers: &[ProviderView]) -> String {
    let Ok(arr) = serde_json::from_str::<Vec<serde_json::Value>>(targets_json) else {
        return "—".into();
    };
    if arr.is_empty() {
        return "—".into();
    }
    let parts: Vec<String> = arr
        .iter()
        .take(3)
        .map(|t| {
            let model = t
                .get("model_id")
                .and_then(|v| v.as_str())
                .unwrap_or("?");
            match TargetBinding::from_target_json(t) {
                TargetBinding::PoolKind { kind } => {
                    let n = providers
                        .iter()
                        .filter(|p| p.kind.eq_ignore_ascii_case(&kind))
                        .count();
                    format!("pool:{kind}×{n}→{model}")
                }
                TargetBinding::Provider { id } => {
                    let name = providers
                        .iter()
                        .find(|p| p.id == id)
                        .map(|p| p.name.as_str())
                        .unwrap_or(if id.is_empty() { "?" } else { id.as_str() });
                    format!("{name}→{model}")
                }
            }
        })
        .collect();
    let extra = arr.len().saturating_sub(3);
    if extra > 0 {
        format!("{}, +{extra}", parts.join(", "))
    } else {
        parts.join(", ")
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
    /// True while a status poll is in flight, so `on_tick` doesn't stack a new
    /// poll on top of a slow/hung one (which would let dozens accumulate).
    pub poll_inflight: bool,
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
            poll_inflight: false,
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
    // Client-local calendar month (matches usage rollups with tz_offset_minutes).
    let now = Local::now();
    format!("{:04}-{:02}", now.year(), now.month())
}

pub fn shift_period(period: &str, delta: i32) -> String {
    let parts: Vec<_> = period.split('-').collect();
    if parts.len() != 2 {
        return current_period();
    }
    let y: i32 = parts[0].parse().unwrap_or(Local::now().year());
    let m: u32 = parts[1].parse().unwrap_or(Local::now().month());
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

    fn pv(id: &str, name: &str, kind: &str) -> ProviderView {
        ProviderView {
            id: id.into(),
            name: name.into(),
            kind: kind.into(),
            base_url: format!("https://{id}.example"),
            upstream_key_ref: String::new(),
            created_at: String::new(),
            updated_at: String::new(),
        }
    }

    #[test]
    fn route_wizard_builds_targets() {
        let mut w = RouteWizard::create(vec![pv("p1", "oai", "openai")]);
        w.match_alias.value = "gpt-4o".into();
        w.targets[0].model_id.value = "gpt-4o".into();
        let body = w.to_body().unwrap();
        assert_eq!(body.match_alias, "gpt-4o");
        assert_eq!(body.strategy, "fixed");
        let arr = body.targets.as_array().unwrap();
        assert_eq!(arr[0]["provider_id"], "p1");
        assert!(arr[0].get("pool_kind").is_none());
    }

    #[test]
    fn route_wizard_pool_kind_target() {
        let providers = vec![
            pv("c1", "claude-a", "claude-oauth"),
            pv("c2", "claude-b", "claude-oauth"),
            pv("o1", "oai", "openai"),
        ];
        let mut w = RouteWizard::create(providers);
        w.match_alias.value = "claude".into();
        w.targets[0].binding = TargetBinding::PoolKind {
            kind: "claude-oauth".into(),
        };
        w.targets[0].model_id.value = "claude-sonnet-4".into();
        // Pool targets use round_robin / fill_first (strategy_idx 0 / 1), not weighted.
        w.strategy_idx = 0;
        let body = w.to_body().unwrap();
        assert_eq!(body.strategy, "round_robin");
        let t = &body.targets.as_array().unwrap()[0];
        assert_eq!(t["pool_kind"], "claude-oauth");
        assert_eq!(t["provider_kind"], "claude-oauth");
        assert_eq!(t["model_id"], "claude-sonnet-4");
        assert!(
            t.get("provider_id").is_none(),
            "empty provider_id should be omitted: {t}"
        );
        assert!(t.get("base_url").is_none());
    }

    #[test]
    fn route_wizard_cycles_strategy_and_binding() {
        let mut w = RouteWizard::create(vec![
            pv("c1", "a", "claude-oauth"),
            pv("c2", "b", "claude-oauth"),
        ]);
        assert_eq!(w.strategy(), "fixed");
        w.cycle_strategy();
        assert_eq!(w.strategy(), "fallback");
        w.cycle_strategy();
        assert_eq!(w.strategy(), "weighted");
        w.cycle_strategy();
        assert_eq!(w.strategy(), "fixed");

        // 2 singles + 1 pool kind
        assert_eq!(w.binding_options().len(), 3);
        w.cycle_provider(); // c1 -> c2
        w.cycle_provider(); // c2 -> pool claude-oauth
        assert!(matches!(
            &w.targets[0].binding,
            TargetBinding::PoolKind { kind } if kind == "claude-oauth"
        ));
    }

    #[test]
    fn provider_edit_body_uses_name_and_base_url_only() {
        let p = pv("id1", "old", "openai");
        let mut f = ProviderForm::edit(&p);
        assert_eq!(f.fields.len(), 2, "edit form must not include kind field");
        f.fields[0].value = "new".into();
        f.fields[1].value = "https://new.example".into();
        let body = f.to_update_body().unwrap();
        assert_eq!(body.name.as_deref(), Some("new"));
        assert_eq!(body.base_url.as_deref(), Some("https://new.example"));
    }

    fn kv(
        id: &str,
        name: &str,
        rpm: Option<i64>,
        whitelist: serde_json::Value,
        enabled: bool,
    ) -> KeyView {
        KeyView {
            id: id.into(),
            name: name.into(),
            model_whitelist: whitelist,
            rate_limit_rpm: rpm,
            enabled,
            created_at: String::new(),
            updated_at: String::new(),
        }
    }

    #[test]
    fn key_form_create_body() {
        let mut f = KeyForm::create();
        assert!(!f.is_edit());
        f.fields[0].value = "ops".into();
        f.fields[1].value = "120".into();
        f.fields[2].value = "gpt-4o, claude-sonnet".into();
        let body = f.to_create_body().unwrap();
        assert_eq!(body.name, "ops");
        assert_eq!(body.rate_limit_rpm, Some(120));
        assert_eq!(
            body.model_whitelist.as_deref(),
            Some(["gpt-4o".into(), "claude-sonnet".into()].as_slice())
        );
    }

    #[test]
    fn key_form_edit_prefills_and_updates() {
        let k = kv(
            "k1",
            "old",
            Some(60),
            serde_json::json!(["gpt-4o"]),
            true,
        );
        let mut f = KeyForm::edit(&k);
        assert_eq!(f.edit_id.as_deref(), Some("k1"));
        assert_eq!(f.fields.len(), 4);
        assert_eq!(f.fields[0].value, "old");
        assert_eq!(f.fields[1].value, "60");
        assert_eq!(f.fields[2].value, "gpt-4o");
        assert_eq!(f.fields[3].value, "true");

        f.fields[0].value = "renamed".into();
        f.fields[1].value = String::new(); // unlimited
        f.fields[2].value = String::new(); // allow all
        f.cycle_enabled();
        assert_eq!(f.fields[3].value, "false");

        let body = f.to_update_body().unwrap();
        assert_eq!(body.name.as_deref(), Some("renamed"));
        assert_eq!(body.rate_limit_rpm, None);
        assert_eq!(body.model_whitelist.as_deref(), Some([].as_slice()));
        assert_eq!(body.enabled, Some(false));
    }

    #[test]
    fn key_form_rejects_non_positive_rpm() {
        let mut f = KeyForm::create();
        f.fields[0].value = "x".into();
        f.fields[1].value = "0".into();
        assert!(f.to_create_body().unwrap_err().contains("positive"));
    }

    #[test]
    fn route_edit_preserves_provider_id_when_list_loads_later() {
        let route = RouteView {
            id: "r1".into(),
            match_alias: "gpt-4o".into(),
            strategy: "fixed".into(),
            targets_json: r#"[{"provider_id":"p2","model_id":"m","provider_kind":"openai"}]"#
                .into(),
            retry_policy_json: String::new(),
            enabled: true,
            created_at: String::new(),
            updated_at: String::new(),
        };
        let mut w = RouteWizard::edit(&route, vec![]);
        assert_eq!(
            w.targets[0].binding,
            TargetBinding::Provider { id: "p2".into() }
        );
        w.set_providers(vec![pv("p1", "a", "openai"), pv("p2", "b", "openai")]);
        assert_eq!(
            w.targets[0].binding,
            TargetBinding::Provider { id: "p2".into() }
        );
        w.targets[0].model_id.value = "m".into();
        let body = w.to_body().unwrap();
        assert_eq!(body.targets[0]["provider_id"], "p2");
    }

    #[test]
    fn route_edit_preserves_pool_kind() {
        let route = RouteView {
            id: "r1".into(),
            match_alias: "sonnet".into(),
            // Pool routes store pool schedule (round_robin / fill_first), not weighted.
            strategy: "round_robin".into(),
            targets_json:
                r#"[{"pool_kind":"claude-oauth","model_id":"claude-sonnet-4","provider_kind":"claude-oauth"}]"#
                    .into(),
            retry_policy_json: String::new(),
            enabled: true,
            created_at: String::new(),
            updated_at: String::new(),
        };
        let w = RouteWizard::edit(
            &route,
            vec![pv("c1", "a", "claude-oauth"), pv("c2", "b", "claude-oauth")],
        );
        assert_eq!(w.strategy(), "round_robin");
        assert_eq!(
            w.targets[0].binding,
            TargetBinding::PoolKind {
                kind: "claude-oauth".into()
            }
        );
        let body = w.to_body().unwrap();
        assert_eq!(body.targets[0]["pool_kind"], "claude-oauth");
    }

    #[test]
    fn summarize_route_targets_shows_pool() {
        let providers = vec![pv("c1", "a", "claude-oauth"), pv("c2", "b", "claude-oauth")];
        let s = summarize_route_targets(
            r#"[{"pool_kind":"claude-oauth","model_id":"m"}]"#,
            &providers,
        );
        assert!(s.contains("pool:claude-oauth×2"), "{s}");
        assert!(s.contains("→m"), "{s}");
    }

    #[test]
    fn shift_period_wraps_year() {
        assert_eq!(shift_period("2026-01", -1), "2025-12");
        assert_eq!(shift_period("2025-12", 1), "2026-01");
    }
}
