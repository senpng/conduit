//! Shared daemon state injected into all axum route handlers.

use std::{
    collections::HashMap,
    path::PathBuf,
    sync::Arc,
};

use arc_swap::ArcSwap;
use conduit_ir::pricing::pricing_kind_aliases;
use conduit_pipeline::{egress::ModelPricing, handle::PipelineHandle};
use conduit_router::table::RoutingTable;
use conduit_secret::SecretBackend;
use conduit_store::{PricingRepo, StorePool};

use crate::oauth::OAuthRuntime;

/// In-memory pricing map: (provider_kind, model_id) → rates.
/// Hot-reloaded via [`ArcSwap::store`]; pipeline lookups are pure sync.
pub type PricingMap = HashMap<(String, String), ModelPricing>;

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

    /// Application data directory (for hot-reload of pricing.json).
    pub data_dir: PathBuf,

    /// OAuth login sessions + callback servers.
    pub oauth: Arc<OAuthRuntime>,

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

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_price(input: f64, output: f64) -> ModelPricing {
        ModelPricing {
            input_per_mtok: input,
            output_per_mtok: output,
            cache_read_per_mtok: None,
            cache_write_per_mtok: None,
            reasoning_per_mtok: None,
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
}
