use std::{collections::HashMap, sync::Arc};

use serde::{Deserialize, Serialize};

use crate::policy::RetryPolicy;

// ---------------------------------------------------------------------------
// Routing strategy
// ---------------------------------------------------------------------------

/// How the router picks a target for a given attempt number.
///
/// Sticky affinity (last successful provider per key+alias) is a **cross-cutting
/// preference** applied to [`Fallback`] and [`Weighted`], not a standalone strategy.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RoutingStrategy {
    /// Always use `targets[0]`. Ignores sticky pin and weights.
    Fixed,
    /// Ordered failover: `targets` order (or sticky-preferred first, then the rest).
    /// Attempt `n` uses the n-th entry, clamping to the last.
    Fallback,
    /// Weighted load balance on attempt 0 (respects sticky pin when present);
    /// later attempts walk remaining targets in table order.
    Weighted,
}

// ---------------------------------------------------------------------------
// Route target
// ---------------------------------------------------------------------------

fn default_weight() -> u32 {
    1
}

/// A single upstream target (provider + model + key binding).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RouteTarget {
    /// Logical provider identifier (e.g. `"openai"`, `"anthropic"`).
    pub provider_id: String,
    /// The provider's model identifier (e.g. `"gpt-4o"`, `"claude-3-5-sonnet-20241022"`).
    pub model_id: String,
    /// Opaque key binding identifier (resolved to an actual secret at call time).
    pub upstream_key_id: String,
    /// Provider kind used to select the correct codec/adapter (e.g. `"openai"`, `"anthropic"`).
    pub provider_kind: String,
    /// Base URL for the upstream provider API (e.g. `"https://api.openai.com"`).
    /// Populated from ProviderRow at route load time. Defaults to None (codec fills in the default).
    #[serde(default)]
    pub base_url: Option<String>,
    /// Relative weight for [`RoutingStrategy::Weighted`] (default 1). Zero = skip for LB pick.
    #[serde(default = "default_weight")]
    pub weight: u32,
}

// ---------------------------------------------------------------------------
// Route definition
// ---------------------------------------------------------------------------

/// A named route that maps an alias to one or more targets.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Route {
    /// The virtual model alias callers use (e.g. `"gpt-4o"`, `"fast"`).
    pub alias: String,
    pub strategy: RoutingStrategy,
    /// Ordered list of targets. Must be non-empty.
    pub targets: Vec<RouteTarget>,
    pub retry_policy: RetryPolicy,
}

// ---------------------------------------------------------------------------
// RoutingTable
// ---------------------------------------------------------------------------

/// An immutable snapshot of routing configuration. Wrap in `Arc` for cheap
/// sharing across request tasks.
///
/// ```text
/// let table = Arc::new(RoutingTable::new(routes));
/// // share across tasks:
/// let t2 = Arc::clone(&table);
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoutingTable {
    /// Map from alias → Route. Keys are lowercase-normalised.
    routes: HashMap<String, Route>,
}

impl RoutingTable {
    /// Build a new table from a list of routes.
    ///
    /// Aliases are normalised to lowercase. Duplicate aliases are overwritten
    /// (last wins — callers are expected to validate uniqueness beforehand).
    pub fn new(routes: impl IntoIterator<Item = Route>) -> Self {
        let map = routes
            .into_iter()
            .map(|r| (r.alias.to_lowercase(), r))
            .collect();
        Self { routes: map }
    }

    /// Retrieve a route by alias (case-insensitive).
    pub fn get(&self, alias: &str) -> Option<&Route> {
        self.routes.get(&alias.to_lowercase())
    }

    /// Number of routes in the table.
    pub fn len(&self) -> usize {
        self.routes.len()
    }

    /// Returns `true` when no routes are configured.
    pub fn is_empty(&self) -> bool {
        self.routes.is_empty()
    }

    /// Iterate over all routes (order not guaranteed).
    pub fn iter(&self) -> impl Iterator<Item = &Route> {
        self.routes.values()
    }

    /// Convenience: wrap `self` in an `Arc` for cheap cloning.
    pub fn into_arc(self) -> Arc<Self> {
        Arc::new(self)
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::policy::RetryPolicy;

    fn make_target(provider: &str, model: &str) -> RouteTarget {
        RouteTarget {
            provider_id: provider.into(),
            model_id: model.into(),
            upstream_key_id: format!("key_{provider}"),
            provider_kind: provider.into(),
            base_url: None,
            weight: 1,
        }
    }

    fn default_retry() -> RetryPolicy {
        RetryPolicy::default()
    }

    #[test]
    fn table_lookup_case_insensitive() {
        let route = Route {
            alias: "GPT-4O".into(),
            strategy: RoutingStrategy::Fixed,
            targets: vec![make_target("openai", "gpt-4o")],
            retry_policy: default_retry(),
        };
        let table = RoutingTable::new([route]);
        assert!(table.get("gpt-4o").is_some());
        assert!(table.get("GPT-4O").is_some());
        assert!(table.get("nonexistent").is_none());
    }

    #[test]
    fn table_len_and_empty() {
        let t0 = RoutingTable::new([]);
        assert!(t0.is_empty());
        assert_eq!(t0.len(), 0);

        let t1 = RoutingTable::new([Route {
            alias: "fast".into(),
            strategy: RoutingStrategy::Fixed,
            targets: vec![make_target("openai", "gpt-4o-mini")],
            retry_policy: default_retry(),
        }]);
        assert!(!t1.is_empty());
        assert_eq!(t1.len(), 1);
    }

    #[test]
    fn table_into_arc() {
        let table = RoutingTable::new([]).into_arc();
        let _clone = Arc::clone(&table);
        assert_eq!(Arc::strong_count(&table), 2);
    }

    #[test]
    fn table_duplicate_alias_last_wins() {
        let r1 = Route {
            alias: "fast".into(),
            strategy: RoutingStrategy::Fixed,
            targets: vec![make_target("openai", "gpt-4o-mini")],
            retry_policy: default_retry(),
        };
        let r2 = Route {
            alias: "fast".into(),
            strategy: RoutingStrategy::Fixed,
            targets: vec![make_target("anthropic", "claude-haiku")],
            retry_policy: default_retry(),
        };
        let table = RoutingTable::new([r1, r2]);
        assert_eq!(table.len(), 1);
        assert_eq!(
            table.get("fast").unwrap().targets[0].provider_id,
            "anthropic"
        );
    }

    #[test]
    fn routing_strategy_serde() {
        for s in [
            RoutingStrategy::Fallback,
            RoutingStrategy::Weighted,
            RoutingStrategy::Fixed,
        ] {
            let j = serde_json::to_string(&s).unwrap();
            let back: RoutingStrategy = serde_json::from_str(&j).unwrap();
            assert_eq!(s, back);
        }
    }

    #[test]
    fn target_weight_defaults_to_one() {
        let v = serde_json::json!({
            "provider_id": "p",
            "model_id": "m",
            "upstream_key_id": "k",
            "provider_kind": "openai"
        });
        let t: RouteTarget = serde_json::from_value(v).unwrap();
        assert_eq!(t.weight, 1);
    }
}
