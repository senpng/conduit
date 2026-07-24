//! Route wizard and target binding helpers.

use crate::dto::{CreateRouteBody, ProviderView, RouteTargetSpec, RouteView};

use super::super::input::InputField;

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


