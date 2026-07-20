//! Shared daemon state injected into all axum route handlers.

use std::{
    collections::HashMap,
    path::PathBuf,
    sync::Arc,
};

use arc_swap::ArcSwap;
use conduit_ir::pricing::pricing_kind_aliases;
use conduit_pipeline::{egress::ModelPricing, handle::PipelineHandle};
use conduit_router::{table::RoutingTable, ProviderCooldownStore, UpstreamQuotaStore};
use conduit_secret::SecretBackend;
use conduit_store::{LimitsRepo, ModelLimitsRow, PricingRepo, StorePool};

use crate::oauth::OAuthRuntime;

/// In-memory pricing map: (provider_kind, model_id) → rates.
/// Hot-reloaded via [`ArcSwap::store`]; pipeline lookups are pure sync.
pub type PricingMap = HashMap<(String, String), ModelPricing>;

/// In-memory model-limits map: (provider_kind, model_id) → context/output limits.
pub type LimitsMap = HashMap<(String, String), ModelLimitsRow>;

/// Runtime state shared by gateway and console handlers.
///
/// Quota / secret / pricing / key-policy resolvers live on the shared
/// [`PipelineHandle`] deps (constructed once at startup), not duplicated here.
pub struct DaemonState {
    /// Current routing table — lock-free reads via ArcSwap; console reloads `store`.
    pub routing_table: Arc<ArcSwap<RoutingTable>>,

    /// Shared pipeline handle (built once at startup).
    pub pipeline: Arc<PipelineHandle>,

    /// SQLite pool — used directly by console API handlers.
    pub pool: StorePool,

    /// Secret backend — used by console API to store/retrieve upstream secrets.
    pub secret_backend: Arc<dyn SecretBackend>,

    /// Pricing repo — exposed so console can trigger hot-reload.
    pub pricing_repo: Arc<PricingRepo>,

    /// Sync pricing snapshot shared with pipeline `pricing_fn`.
    pub pricing_table: Arc<ArcSwap<PricingMap>>,

    /// Model-limits repo (context window / max output) — separate from pricing.
    pub limits_repo: Arc<LimitsRepo>,

    /// Sync limits snapshot for `/v1/models` (and future clients).
    pub limits_table: Arc<ArcSwap<LimitsMap>>,

    /// Application data directory (for hot-reload of pricing.json).
    pub data_dir: PathBuf,

    /// OAuth login sessions + callback servers.
    pub oauth: Arc<OAuthRuntime>,

    /// Daemon config proxy (CLIProxyAPI `cfg.ProxyURL`); env / per-cred override.
    pub proxy_url: Option<String>,

    /// Upstream provider cooldown after 429 / usage_limit.
    pub cooldown: Arc<ProviderCooldownStore>,

    /// Last-seen upstream rate-limit / quota headers & error signals.
    pub quota_snapshots: Arc<UpstreamQuotaStore>,

    /// Daemon version string.
    pub version: &'static str,
}

/// Build a pricing map from all rows currently in the pricing repo.
pub async fn pricing_map_from_repo(repo: &PricingRepo) -> PricingMap {
    let rows = repo.all().await;
    let mut map = PricingMap::with_capacity(rows.len());
    for r in rows {
        map.insert(
            (r.provider_kind, r.model_id),
            ModelPricing {
                input_per_mtok: r.input_per_mtok,
                output_per_mtok: r.output_per_mtok,
                cache_read_per_mtok: r.cache_read_per_mtok,
                cache_write_per_mtok: r.cache_write_per_mtok,
                reasoning_per_mtok: r.reasoning_per_mtok,
            },
        );
    }
    map
}

/// Build a limits map from all rows currently in the limits repo.
pub async fn limits_map_from_repo(repo: &LimitsRepo) -> LimitsMap {
    let rows = repo.all().await;
    let mut map = LimitsMap::with_capacity(rows.len());
    for r in rows {
        map.insert((r.provider_kind.clone(), r.model_id.clone()), r);
    }
    map
}

/// Hot-path pricing lookup with kind aliases + model prefix fallback.
///
/// The ArcSwap map is exact-key only; this mirrors PricingRepo snapshot
/// fallback so OAuth kinds (`grok-oauth`) and dated model ids still resolve.
pub fn lookup_pricing(map: &PricingMap, kind: &str, model: &str) -> Option<ModelPricing> {
    let kind = kind.trim();
    let model = model.trim();
    if let Some(p) = map.get(&(kind.to_string(), model.to_string())) {
        return Some(p.clone());
    }
    for alt in pricing_kind_aliases(kind) {
        if let Some(p) = map.get(&(alt.to_string(), model.to_string())) {
            return Some(p.clone());
        }
    }
    // Prefix: "grok-4.5-build" ↔ "grok-4.5"
    for alt in std::iter::once(kind).chain(pricing_kind_aliases(kind).iter().copied()) {
        if let Some((_, p)) = map
            .iter()
            .find(|((k, m), _)| k == alt && (model.starts_with(m.as_str()) || m.starts_with(model)))
        {
            return Some(p.clone());
        }
    }
    // Last resort: any row with the same model_id
    map.iter()
        .find(|((_, m), _)| m == model)
        .map(|(_, p)| p.clone())
}

/// Hot-path limits lookup (same fallback order as pricing / store snapshot).
pub fn lookup_limits(map: &LimitsMap, kind: &str, model: &str) -> Option<ModelLimitsRow> {
    // Reuse store pure helper over a temporary vec would allocate; mirror map path.
    let kind = kind.trim();
    let model = model.trim();
    if kind.is_empty() || model.is_empty() {
        return None;
    }
    if let Some(r) = map.get(&(kind.to_string(), model.to_string())) {
        return Some(r.clone());
    }
    for alt in pricing_kind_aliases(kind) {
        if let Some(r) = map.get(&(alt.to_string(), model.to_string())) {
            return Some(r.clone());
        }
    }
    for alt in std::iter::once(kind).chain(pricing_kind_aliases(kind).iter().copied()) {
        if let Some((_, r)) = map.iter().find(|((k, m), _)| {
            k == alt && (model.starts_with(m.as_str()) || m.starts_with(model))
        }) {
            return Some(r.clone());
        }
    }
    map.iter()
        .find(|((_, m), _)| m == model)
        .map(|(_, r)| r.clone())
}

/// Build OpenAI-compatible `/v1/models` `data` entries from routes + limits.
///
/// Emits `context_window` and `context_length` only when a positive limit is
/// known for the route's first target. Does not invent a default window.
pub fn build_models_list_data(
    routes: impl IntoIterator<Item = (String, String, String)>,
    limits: &LimitsMap,
) -> Vec<serde_json::Value> {
    let mut data = Vec::new();
    for (alias, owned_by, model_id) in routes {
        let mut entry = serde_json::json!({
            "id": alias,
            "object": "model",
            "created": 0,
            "owned_by": owned_by,
        });
        if let Some(lim) = lookup_limits(limits, &owned_by, &model_id) {
            if lim.max_input_tokens > 0 {
                entry["context_window"] = serde_json::json!(lim.max_input_tokens);
                entry["context_length"] = serde_json::json!(lim.max_input_tokens);
            }
            if let Some(out) = lim.max_output_tokens.filter(|&n| n > 0) {
                entry["max_completion_tokens"] = serde_json::json!(out);
            }
        }
        data.push(entry);
    }
    data.sort_by(|a, b| {
        a.get("id")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .cmp(b.get("id").and_then(|v| v.as_str()).unwrap_or(""))
    });
    data
}

#[cfg(test)]
mod tests {
    use super::*;
    use conduit_store::lookup_limits_rows;

    fn sample_price(input: f64, output: f64) -> ModelPricing {
        ModelPricing {
            input_per_mtok: input,
            output_per_mtok: output,
            cache_read_per_mtok: None,
            cache_write_per_mtok: None,
            reasoning_per_mtok: None,
        }
    }

    fn sample_limits(kind: &str, model: &str, input: u64, output: Option<u64>) -> ModelLimitsRow {
        ModelLimitsRow {
            provider_kind: kind.into(),
            model_id: model.into(),
            max_input_tokens: input,
            max_output_tokens: output,
        }
    }

    #[test]
    fn lookup_pricing_exact_and_kind_alias() {
        let mut map = PricingMap::new();
        map.insert(
            ("grok-oauth".into(), "grok-4.5".into()),
            sample_price(3.0, 15.0),
        );
        let p = lookup_pricing(&map, "xai", "grok-4.5").unwrap();
        assert_eq!(p.input_per_mtok, 3.0);
    }

    #[test]
    fn lookup_pricing_model_prefix() {
        let mut map = PricingMap::new();
        map.insert(
            ("grok-oauth".into(), "grok-4.5".into()),
            sample_price(3.0, 15.0),
        );
        let p = lookup_pricing(&map, "grok-oauth", "grok-4.5-build-nightly").unwrap();
        assert_eq!(p.output_per_mtok, 15.0);
    }

    #[test]
    fn models_list_includes_window_when_limits_known() {
        let mut limits = LimitsMap::new();
        limits.insert(
            ("xai".into(), "grok-4.5".into()),
            sample_limits("xai", "grok-4.5", 500_000, Some(65_536)),
        );
        let data = build_models_list_data(
            vec![
                ("grok-4.5".into(), "grok-oauth".into(), "grok-4.5".into()),
                ("mystery".into(), "openai".into(), "unknown-model".into()),
            ],
            &limits,
        );
        assert_eq!(data.len(), 2);
        let grok = data.iter().find(|m| m["id"] == "grok-4.5").unwrap();
        assert_eq!(grok["context_window"].as_u64().unwrap(), 500_000);
        assert_eq!(grok["context_length"].as_u64().unwrap(), 500_000);
        assert_eq!(grok["max_completion_tokens"].as_u64().unwrap(), 65_536);

        let mystery = data.iter().find(|m| m["id"] == "mystery").unwrap();
        assert!(mystery.get("context_window").is_none());
        assert!(mystery.get("context_length").is_none());
        // Does not invent 256k
        assert_ne!(mystery.get("context_window").and_then(|v| v.as_u64()), Some(256_000));
    }

    #[test]
    fn lookup_limits_rows_helper_still_used_by_store() {
        // Structural: re-export path compiles; pure path matches map lookup.
        let rows = vec![sample_limits("xai", "grok-4.5", 500_000, None)];
        let via_rows = lookup_limits_rows(&rows, "grok-oauth", "grok-4.5").unwrap();
        let mut map = LimitsMap::new();
        map.insert(
            ("xai".into(), "grok-4.5".into()),
            sample_limits("xai", "grok-4.5", 500_000, None),
        );
        let via_map = lookup_limits(&map, "grok-oauth", "grok-4.5").unwrap();
        assert_eq!(via_rows.max_input_tokens, via_map.max_input_tokens);
    }
}
