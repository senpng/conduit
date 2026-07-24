use std::time::{SystemTime, UNIX_EPOCH};

use thiserror::Error;

use crate::{
    policy::RetryPolicy,
    pool::{expand_route_target, select_among_members, PoolCursorStore},
    table::{RouteTarget, RoutingStrategy, RoutingTable},
};

// ---------------------------------------------------------------------------
// RouterError
// ---------------------------------------------------------------------------

/// Errors returned by the pure routing function.
#[derive(Debug, Error, PartialEq)]
pub enum RouterError {
    #[error("unknown alias '{alias}'")]
    UnknownAlias { alias: String },

    #[error("route '{alias}' has no targets configured")]
    NoTargets { alias: String },

    #[error("attempt {attempt_no} exceeds max retries {max_retries} for alias '{alias}'")]
    AttemptsExhausted {
        alias: String,
        attempt_no: u32,
        max_retries: u32,
    },

    #[error("route '{alias}': {message}")]
    Pool { alias: String, message: String },
}

// ---------------------------------------------------------------------------
// RoutingDecision
// ---------------------------------------------------------------------------

/// The output of a successful routing call.
#[derive(Debug, Clone)]
pub struct RoutingDecision {
    pub provider_id: String,
    pub model_id: String,
    pub provider_kind: String,
    pub base_url: Option<String>,
    pub request_overrides: serde_json::Map<String, serde_json::Value>,
    pub attempt_no: u32,
    pub retry_policy: RetryPolicy,
}

// ---------------------------------------------------------------------------
// route — pure selection (seeded for weighted LB)
// ---------------------------------------------------------------------------

/// Select a target for the given `alias` and `attempt_no`.
///
/// Production entry point: weighted first-picks use a wall-clock seed.
/// For deterministic tests use [`route_with_seed`].
///
/// Session affinity: when `preferred_provider_id` is set and still on the route,
/// it is preferred for attempt 0 (and reorders the retry chain) for
/// [`Fallback`] and [`Weighted`]. Pool routes use [`PoolStrategy`] instead.
///
/// | Strategy   | Behaviour |
/// |------------|-----------|
/// | `Fixed`    | Always `targets[0]`. |
/// | `Fallback` | Session-aware ordered failover. |
/// | `Weighted` | Session pin if present; else weighted pick on attempt 0; retries remaining. |
/// | pool path  | `round_robin` / `fill_first` (+ session pin base layer). |
pub fn route(
    alias: &str,
    table: &RoutingTable,
    attempt_no: u32,
    preferred_provider_id: Option<&str>,
) -> Result<RoutingDecision, RouterError> {
    route_with_options(
        alias,
        table,
        attempt_no,
        preferred_provider_id,
        None,
        None,
        None,
    )
}

/// Same as [`route`] but with an explicit seed for weighted load-balancing.
///
/// Pure with respect to inputs: same `(alias, table, attempt_no, preferred, seed)`
/// always yields the same decision (pool RR without a cursor store starts at 0).
pub fn route_with_seed(
    alias: &str,
    table: &RoutingTable,
    attempt_no: u32,
    preferred_provider_id: Option<&str>,
    seed: u64,
) -> Result<RoutingDecision, RouterError> {
    route_with_options(
        alias,
        table,
        attempt_no,
        preferred_provider_id,
        Some(seed),
        None,
        None,
    )
}

/// Full routing entry: optional seed, cooldown skip set, and pool RR cursors.
///
/// Targets whose `provider_id` is in `skip_provider_ids` are excluded when any
/// non-cooling member remains; if every target is cooling, the full set is used
/// so single-provider routes keep working (caller sees the upstream error).
///
/// Pool paths use [`Route::pool_strategy`] and optional [`PoolCursorStore`].
pub fn route_with_options(
    alias: &str,
    table: &RoutingTable,
    attempt_no: u32,
    preferred_provider_id: Option<&str>,
    seed: Option<u64>,
    skip_provider_ids: Option<&std::collections::HashSet<String>>,
    pool_cursors: Option<&PoolCursorStore>,
) -> Result<RoutingDecision, RouterError> {
    let seed = seed.unwrap_or_else(|| {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0)
    });

    let r = table.get(alias).ok_or_else(|| RouterError::UnknownAlias {
        alias: alias.to_string(),
    })?;

    if r.targets.is_empty() {
        return Err(RouterError::NoTargets {
            alias: alias.to_string(),
        });
    }

    if attempt_no > r.retry_policy.max_retries {
        return Err(RouterError::AttemptsExhausted {
            alias: alias.to_string(),
            attempt_no,
            max_retries: r.retry_policy.max_retries,
        });
    }

    // Expand pool targets into concrete providers; keep single-provider targets as-is.
    let mut expanded: Vec<RouteTarget> = Vec::new();
    for t in &r.targets {
        match expand_route_target(t, &table.providers, &table.pools) {
            Ok(chunk) => expanded.extend(chunk),
            Err(message) => {
                return Err(RouterError::Pool {
                    alias: alias.to_string(),
                    message,
                });
            }
        }
    }
    if expanded.is_empty() {
        return Err(RouterError::NoTargets {
            alias: alias.to_string(),
        });
    }

    // Pool path: session pin + round_robin / fill_first (ignores fixed/fallback/weighted).
    let has_pool = r.targets.iter().any(|t| t.is_pool_target());
    if has_pool {
        let chosen = select_among_members(
            &expanded,
            preferred_provider_id,
            skip_provider_ids,
            attempt_no,
            r.pool_strategy,
            alias,
            pool_cursors,
        )
        .ok_or_else(|| RouterError::NoTargets {
            alias: alias.to_string(),
        })?;
        return Ok(decision_from(
            chosen.target,
            attempt_no,
            r.retry_policy.clone(),
        ));
    }

    // Prefer non-cooling targets; if all cooling, fall back to full target list.
    let available = filter_targets_by_cooldown(&expanded, skip_provider_ids);
    let pool: Vec<&RouteTarget> = if available.is_empty() {
        expanded.iter().collect()
    } else {
        available
    };

    let target = select_from_pool(
        &r.strategy,
        &pool,
        preferred_provider_id,
        attempt_no,
        seed,
    );
    Ok(decision_from(target, attempt_no, r.retry_policy.clone()))
}

fn decision_from(
    target: &RouteTarget,
    attempt_no: u32,
    retry_policy: RetryPolicy,
) -> RoutingDecision {
    RoutingDecision {
        provider_id: target.provider_id.clone(),
        model_id: target.model_id.clone(),
        provider_kind: target.provider_kind.clone(),
        base_url: target.base_url.clone(),
        request_overrides: target.request_overrides.clone(),
        attempt_no,
        retry_policy,
    }
}

fn select_from_pool<'a>(
    strategy: &RoutingStrategy,
    pool: &[&'a RouteTarget],
    preferred_provider_id: Option<&str>,
    attempt_no: u32,
    seed: u64,
) -> &'a RouteTarget {
    if pool.is_empty() {
        unreachable!("caller guarantees non-empty pool");
    }
    let order = match strategy {
        RoutingStrategy::Fixed => return pool[0],
        RoutingStrategy::Fallback => order_sticky(pool, preferred_provider_id),
        RoutingStrategy::Weighted => order_weighted(pool, preferred_provider_id, seed),
    };
    let idx = (attempt_no as usize).min(order.len() - 1);
    order[idx]
}

/// Non-cooling targets first; if `skip` is empty/None, all targets.
fn filter_targets_by_cooldown<'a>(
    targets: &'a [RouteTarget],
    skip: Option<&std::collections::HashSet<String>>,
) -> Vec<&'a RouteTarget> {
    let Some(skip) = skip.filter(|s| !s.is_empty()) else {
        return targets.iter().collect();
    };
    targets
        .iter()
        .filter(|t| !skip.contains(&t.provider_id))
        .collect()
}

/// Move `targets[first_idx]` to the front; preserve relative order of the rest.
fn reorder_first_at<'a>(targets: &[&'a RouteTarget], first_idx: usize) -> Vec<&'a RouteTarget> {
    if targets.is_empty() {
        return Vec::new();
    }
    let first_idx = first_idx.min(targets.len() - 1);
    let mut out = Vec::with_capacity(targets.len());
    out.push(targets[first_idx]);
    out.extend(
        targets
            .iter()
            .enumerate()
            .filter(|(i, _)| *i != first_idx)
            .map(|(_, t)| *t),
    );
    out
}

/// Preferred provider first (if still listed), else table order.
fn order_sticky<'a>(
    targets: &[&'a RouteTarget],
    preferred_provider_id: Option<&str>,
) -> Vec<&'a RouteTarget> {
    if let Some(pref) = preferred_provider_id.filter(|s| !s.is_empty()) {
        if let Some(pin_idx) = targets.iter().position(|t| t.provider_id == pref) {
            return reorder_first_at(targets, pin_idx);
        }
    }
    targets.to_vec()
}

/// Sticky pin wins when present; otherwise weight-pick the first attempt target.
fn order_weighted<'a>(
    targets: &[&'a RouteTarget],
    preferred_provider_id: Option<&str>,
    seed: u64,
) -> Vec<&'a RouteTarget> {
    if let Some(pref) = preferred_provider_id.filter(|s| !s.is_empty()) {
        if let Some(pin_idx) = targets.iter().position(|t| t.provider_id == pref) {
            return reorder_first_at(targets, pin_idx);
        }
    }
    let first_idx =
        pick_weighted_among(targets.iter().map(|t| t.weight), targets.len(), seed);
    reorder_first_at(targets, first_idx)
}

/// Weighted index from a weight sequence. Zero-weight entries are skipped; if
/// all weights are zero, falls back to `seed % len`.
fn pick_weighted_among(
    weights: impl Iterator<Item = u32> + Clone,
    len: usize,
    seed: u64,
) -> usize {
    if len == 0 {
        return 0;
    }
    let total: u64 = weights.clone().map(u64::from).sum();
    if total == 0 {
        return (seed as usize) % len;
    }
    let mut r = seed % total;
    for (i, w) in weights.map(u64::from).enumerate() {
        if r < w {
            return i;
        }
        r -= w;
    }
    len - 1
}

/// Preferred provider first (if still listed), then remaining targets in table order.
///
/// Shared by sticky fallback / affinity and as the pin path for weighted LB.
pub fn sticky_target_order<'a>(
    targets: &'a [RouteTarget],
    preferred_provider_id: Option<&str>,
) -> Vec<&'a RouteTarget> {
    let refs: Vec<&RouteTarget> = targets.iter().collect();
    order_sticky(&refs, preferred_provider_id)
}

/// Build attempt order for weighted LB: sticky pin wins; else weight-pick first.
pub fn weighted_target_order<'a>(
    targets: &'a [RouteTarget],
    preferred_provider_id: Option<&str>,
    seed: u64,
) -> Vec<&'a RouteTarget> {
    let refs: Vec<&RouteTarget> = targets.iter().collect();
    order_weighted(&refs, preferred_provider_id, seed)
}

/// Weighted index among targets. Zero-weight targets are skipped; if all zero,
/// falls back to `seed % len`.
pub fn pick_weighted_index(targets: &[RouteTarget], seed: u64) -> usize {
    pick_weighted_among(targets.iter().map(|t| t.weight), targets.len(), seed)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        affinity::AffinityStore,
        policy::RetryPolicy,
        pool::{auto_kind_pools, PoolCursorStore, PoolStrategy, ProviderCatalogEntry},
        table::{Route, RouteTarget, RoutingStrategy, RoutingTable},
    };

    fn target(provider: &str, model: &str) -> RouteTarget {
        target_w(provider, model, 1)
    }

    fn target_w(provider: &str, model: &str, weight: u32) -> RouteTarget {
        RouteTarget {
            provider_id: provider.into(),
            model_id: model.into(),
            provider_kind: provider.into(),
            base_url: None,
            weight,
            request_overrides: Default::default(),
            pool_id: None,
            pool_kind: None,
        }
    }

    fn retry(max: u32) -> RetryPolicy {
        RetryPolicy {
            max_retries: max,
            ..Default::default()
        }
    }

    fn fixed_route(alias: &str, targets: Vec<RouteTarget>) -> Route {
        Route {
            alias: alias.into(),
            strategy: RoutingStrategy::Fixed,
            pool_strategy: PoolStrategy::default(),
            targets,
            retry_policy: retry(2),
        }
    }

    fn fallback_route(alias: &str, targets: Vec<RouteTarget>) -> Route {
        Route {
            alias: alias.into(),
            strategy: RoutingStrategy::Fallback,
            pool_strategy: PoolStrategy::default(),
            targets,
            retry_policy: retry(3),
        }
    }

    fn weighted_route(alias: &str, targets: Vec<RouteTarget>) -> Route {
        Route {
            alias: alias.into(),
            strategy: RoutingStrategy::Weighted,
            pool_strategy: PoolStrategy::default(),
            targets,
            retry_policy: retry(3),
        }
    }

    // -----------------------------------------------------------------------
    // Fixed
    // -----------------------------------------------------------------------

    #[test]
    fn fixed_always_picks_first_target() {
        let table = RoutingTable::new([fixed_route(
            "fast",
            vec![
                target("openai", "gpt-4o-mini"),
                target("anthropic", "claude-haiku"),
            ],
        )]);

        for attempt in 0..=2 {
            let d = route_with_seed("fast", &table, attempt, None, 0).unwrap();
            assert_eq!(d.provider_id, "openai");
            assert_eq!(d.model_id, "gpt-4o-mini");
        }
    }

    #[test]
    fn fixed_ignores_preferred_pin() {
        let table = RoutingTable::new([fixed_route(
            "fast",
            vec![target("openai", "gpt-4o"), target("anthropic", "claude")],
        )]);
        let d = route_with_seed("fast", &table, 0, Some("anthropic"), 0).unwrap();
        assert_eq!(d.provider_id, "openai");
    }

    // -----------------------------------------------------------------------
    // Fallback + sticky
    // -----------------------------------------------------------------------

    #[test]
    fn fallback_without_pin_uses_table_order() {
        let table = RoutingTable::new([fallback_route(
            "smart",
            vec![
                target("openai", "gpt-4o"),
                target("anthropic", "claude-sonnet"),
                target("gemini", "gemini-2.0-flash"),
            ],
        )]);

        assert_eq!(
            route_with_seed("smart", &table, 0, None, 0)
                .unwrap()
                .provider_id,
            "openai"
        );
        assert_eq!(
            route_with_seed("smart", &table, 1, None, 0)
                .unwrap()
                .provider_id,
            "anthropic"
        );
        assert_eq!(
            route_with_seed("smart", &table, 2, None, 0)
                .unwrap()
                .provider_id,
            "gemini"
        );
    }

    #[test]
    fn fallback_with_pin_preferred_first_then_remaining() {
        let table = RoutingTable::new([fallback_route(
            "smart",
            vec![target("p0", "m0"), target("p1", "m1"), target("p2", "m2")],
        )]);
        // Prefer p1 → order p1, p0, p2
        assert_eq!(
            route_with_seed("smart", &table, 0, Some("p1"), 0)
                .unwrap()
                .provider_id,
            "p1"
        );
        assert_eq!(
            route_with_seed("smart", &table, 1, Some("p1"), 0)
                .unwrap()
                .provider_id,
            "p0"
        );
        assert_eq!(
            route_with_seed("smart", &table, 2, Some("p1"), 0)
                .unwrap()
                .provider_id,
            "p2"
        );
    }

    #[test]
    fn fallback_unknown_preferred_uses_table_order() {
        let table = RoutingTable::new([fallback_route(
            "smart",
            vec![target("p0", "m0"), target("p1", "m1")],
        )]);
        assert_eq!(
            route_with_seed("smart", &table, 0, Some("gone"), 0)
                .unwrap()
                .provider_id,
            "p0"
        );
    }

    #[test]
    fn fallback_clamps_to_last_target() {
        let table = RoutingTable::new([fallback_route(
            "smart",
            vec![target("openai", "gpt-4o"), target("anthropic", "claude")],
        )]);
        assert_eq!(
            route_with_seed("smart", &table, 2, None, 0)
                .unwrap()
                .provider_id,
            "anthropic"
        );
        assert_eq!(
            route_with_seed("smart", &table, 3, None, 0)
                .unwrap()
                .provider_id,
            "anthropic"
        );
    }

    // -----------------------------------------------------------------------
    // Weighted + sticky
    // -----------------------------------------------------------------------

    #[test]
    fn pick_weighted_index_respects_relative_weights() {
        // weight 0 vs 100 → always index 1 for any seed
        let targets = vec![target_w("a", "m", 0), target_w("b", "m", 100)];
        for seed in 0..50u64 {
            assert_eq!(
                pick_weighted_index(&targets, seed),
                1,
                "seed {seed} should pick weight-100 target"
            );
        }
    }

    #[test]
    fn pick_weighted_all_zero_weights_uses_seed_mod_len() {
        let targets = vec![target_w("a", "m", 0), target_w("b", "m", 0), target_w("c", "m", 0)];
        assert_eq!(pick_weighted_index(&targets, 0), 0);
        assert_eq!(pick_weighted_index(&targets, 1), 1);
        assert_eq!(pick_weighted_index(&targets, 2), 2);
        assert_eq!(pick_weighted_index(&targets, 5), 2); // 5 % 3
        // Production path (cooldown-filtered refs → order_weighted) must match.
        let table = RoutingTable::new([weighted_route("lb", targets)]);
        assert_eq!(
            route_with_seed("lb", &table, 0, None, 1)
                .unwrap()
                .provider_id,
            "b"
        );
    }

    #[test]
    fn weighted_first_attempt_respects_weights_without_pin() {
        let table = RoutingTable::new([weighted_route(
            "lb",
            vec![target_w("low", "m", 1), target_w("high", "m", 10_000)],
        )]);
        // seed that falls into the high bucket: total=10001, seed%10001 >= 1 → high
        let d = route_with_seed("lb", &table, 0, None, 1).unwrap();
        assert_eq!(d.provider_id, "high");
        // seed 0 lands in the low bucket (r < 1)
        let d0 = route_with_seed("lb", &table, 0, None, 0).unwrap();
        assert_eq!(d0.provider_id, "low");
    }

    #[test]
    fn weighted_attempt_zero_with_pin_returns_preferred() {
        let table = RoutingTable::new([weighted_route(
            "lb",
            vec![
                target_w("p0", "m", 1000),
                target_w("p1", "m", 1),
                target_w("p2", "m", 1),
            ],
        )]);
        // Even with seed that would pick p0 by weight, pin wins.
        let d = route_with_seed("lb", &table, 0, Some("p1"), 0).unwrap();
        assert_eq!(d.provider_id, "p1");
        let d1 = route_with_seed("lb", &table, 1, Some("p1"), 0).unwrap();
        assert_eq!(d1.provider_id, "p0");
    }

    #[test]
    fn weighted_unknown_preferred_falls_back_to_weight_pick() {
        let table = RoutingTable::new([weighted_route("lb", vec![target_w("only", "m", 1)])]);
        let d = route_with_seed("lb", &table, 0, Some("gone"), 42).unwrap();
        assert_eq!(d.provider_id, "only");
    }

    #[test]
    fn weighted_retries_walk_remaining_after_first_pick() {
        let table = RoutingTable::new([weighted_route(
            "lb",
            vec![
                target_w("a", "m", 0),
                target_w("b", "m", 100),
                target_w("c", "m", 0),
            ],
        )]);
        // seed any → b first (only non-zero weight)
        assert_eq!(
            route_with_seed("lb", &table, 0, None, 7)
                .unwrap()
                .provider_id,
            "b"
        );
        assert_eq!(
            route_with_seed("lb", &table, 1, None, 7)
                .unwrap()
                .provider_id,
            "a"
        );
        assert_eq!(
            route_with_seed("lb", &table, 2, None, 7)
                .unwrap()
                .provider_id,
            "c"
        );
    }

    /// Without a request-scoped seed, re-sampling on attempt 1 can reselect the
    /// failed attempt-0 provider (equal weights). Production must pin one seed.
    #[test]
    fn weighted_different_seeds_can_reselect_attempt_zero_provider() {
        // Equal weights → pick = seed % 2.
        // seed0: order [a,b] → attempt0=a
        // seed1: order [b,a] → attempt1=a  (reselects failed a)
        let targets = vec![target_w("a", "m", 1), target_w("b", "m", 1)];
        assert_eq!(pick_weighted_index(&targets, 0), 0);
        assert_eq!(pick_weighted_index(&targets, 1), 1);
        let table = RoutingTable::new([weighted_route("lb", targets)]);
        let first = route_with_seed("lb", &table, 0, None, 0)
            .unwrap()
            .provider_id;
        let again = route_with_seed("lb", &table, 1, None, 1)
            .unwrap()
            .provider_id;
        assert_eq!(first, "a");
        assert_eq!(
            again, "a",
            "different seed on retry can reselect attempt-0 provider (why seed is request-scoped)"
        );
    }

    /// Same seed across attempts: attempt 1 never equals attempt 0 when ≥2 targets.
    #[test]
    fn weighted_same_seed_retry_never_reselects_first_of_two() {
        let table = RoutingTable::new([weighted_route(
            "lb",
            vec![target_w("a", "m", 1), target_w("b", "m", 1)],
        )]);
        for seed in 0..32u64 {
            let p0 = route_with_seed("lb", &table, 0, None, seed)
                .unwrap()
                .provider_id;
            let p1 = route_with_seed("lb", &table, 1, None, seed)
                .unwrap()
                .provider_id;
            assert_ne!(
                p0, p1,
                "seed {seed}: retry must use remaining target, got {p0} then {p1}"
            );
        }
    }

    // -----------------------------------------------------------------------
    // Affinity store + real route path (success pin → next attempt-0)
    // -----------------------------------------------------------------------

    #[test]
    fn affinity_store_pin_drives_fallback_and_weighted_attempt_zero() {
        let store = AffinityStore::new();
        let table = RoutingTable::new([
            fallback_route(
                "fb",
                vec![target("p0", "m"), target("p1", "m"), target("p2", "m")],
            ),
            weighted_route(
                "lb",
                vec![
                    target_w("p0", "m", 1000),
                    target_w("p1", "m", 1),
                    target_w("p2", "m", 1),
                ],
            ),
        ]);

        // Simulate success on p2 for session s-a
        store.remember("s-a", "fb", "p2");
        store.remember("s-a", "lb", "p2");

        let pref_fb = store.preferred("s-a", "fb");
        assert_eq!(
            route_with_seed("fb", &table, 0, pref_fb.as_deref(), 0)
                .unwrap()
                .provider_id,
            "p2"
        );

        let pref_lb = store.preferred("s-a", "lb");
        assert_eq!(
            route_with_seed("lb", &table, 0, pref_lb.as_deref(), 0)
                .unwrap()
                .provider_id,
            "p2"
        );
    }

    // -----------------------------------------------------------------------
    // Errors / fields
    // -----------------------------------------------------------------------

    #[test]
    fn unknown_alias_returns_error() {
        let table = RoutingTable::new([fixed_route("fast", vec![target("openai", "gpt-4o")])]);
        let err = route_with_seed("nonexistent", &table, 0, None, 0).unwrap_err();
        assert!(matches!(err, RouterError::UnknownAlias { .. }));
    }

    #[test]
    fn empty_targets_returns_error() {
        let table = RoutingTable::new([Route {
            alias: "empty".into(),
            strategy: RoutingStrategy::Fixed,
            pool_strategy: PoolStrategy::default(),
            targets: vec![],
            retry_policy: retry(2),
        }]);
        let err = route_with_seed("empty", &table, 0, None, 0).unwrap_err();
        assert!(matches!(err, RouterError::NoTargets { .. }));
    }

    // -----------------------------------------------------------------------
    // Provider pools (scheme B) + sticky + cooldown
    // -----------------------------------------------------------------------

    fn catalog_entry(id: &str, kind: &str) -> ProviderCatalogEntry {
        ProviderCatalogEntry {
            id: id.into(),
            kind: kind.into(),
            base_url: Some(format!("https://{id}.example")),
            weight: 1,
        }
    }

    fn pool_route_table(mode: PoolStrategy) -> RoutingTable {
        let providers = vec![
            catalog_entry("c1", "claude-oauth"),
            catalog_entry("c2", "claude-oauth"),
            catalog_entry("o1", "openai"),
        ];
        let pools = auto_kind_pools(&providers);
        let route = Route {
            alias: "claude".into(),
            strategy: RoutingStrategy::Fallback, // ignored for pool member pick
            pool_strategy: mode,
            targets: vec![RouteTarget {
                provider_id: String::new(),
                model_id: "claude-sonnet".into(),
                provider_kind: "claude-oauth".into(),
                base_url: None,
                weight: 1,
                request_overrides: Default::default(),
                pool_id: None,
                pool_kind: Some("claude-oauth".into()),
            }],
            retry_policy: retry(2),
        };
        RoutingTable::new([route]).with_provider_catalog(providers, pools)
    }

    #[test]
    fn pool_kind_expands_and_picks_member() {
        let table = pool_route_table(PoolStrategy::FillFirst);
        let d = route_with_seed("claude", &table, 0, None, 0).unwrap();
        // fill-first stable order → c1
        assert_eq!(d.provider_id, "c1");
        assert_eq!(d.model_id, "claude-sonnet");
        assert_eq!(d.provider_kind, "claude-oauth");
    }

    #[test]
    fn pool_session_pin_when_available() {
        let table = pool_route_table(PoolStrategy::RoundRobin);
        let store = AffinityStore::new();
        store.remember("sess-1", "claude", "c2");
        let pref = store.preferred("sess-1", "claude");
        let d = route_with_seed("claude", &table, 0, pref.as_deref(), 99).unwrap();
        assert_eq!(d.provider_id, "c2");
    }

    #[test]
    fn pool_session_pin_ignored_when_cooling() {
        let table = pool_route_table(PoolStrategy::FillFirst);
        let mut skip = std::collections::HashSet::new();
        skip.insert("c2".into());
        let d = route_with_options(
            "claude",
            &table,
            0,
            Some("c2"),
            Some(0),
            Some(&skip),
            None,
        )
        .unwrap();
        assert_eq!(d.provider_id, "c1");
    }

    #[test]
    fn pool_round_robin_rotates_with_cursor_store() {
        let table = pool_route_table(PoolStrategy::RoundRobin);
        let cursors = PoolCursorStore::new();
        let mut seen = Vec::new();
        for _ in 0..2 {
            let d = route_with_options("claude", &table, 0, None, Some(0), None, Some(&cursors))
                .unwrap();
            seen.push(d.provider_id);
        }
        assert_eq!(seen, vec!["c1", "c2"]);
    }

    #[test]
    fn pool_fill_first_stable() {
        let table = pool_route_table(PoolStrategy::FillFirst);
        for _ in 0..3 {
            let d = route_with_seed("claude", &table, 0, None, 0).unwrap();
            assert_eq!(d.provider_id, "c1");
        }
    }

    #[test]
    fn pool_session_pin_shared_across_aliases() {
        let providers = vec![
            catalog_entry("c1", "claude-oauth"),
            catalog_entry("c2", "claude-oauth"),
        ];
        let pools = auto_kind_pools(&providers);
        let mk = |alias: &str| Route {
            alias: alias.into(),
            strategy: RoutingStrategy::Fixed,
            pool_strategy: PoolStrategy::RoundRobin,
            targets: vec![RouteTarget {
                provider_id: String::new(),
                model_id: "m".into(),
                provider_kind: "claude-oauth".into(),
                base_url: None,
                weight: 1,
                request_overrides: Default::default(),
                pool_id: Some("claude-oauth".into()),
                pool_kind: None,
            }],
            retry_policy: retry(1),
        };
        let table = RoutingTable::new([mk("alias-a"), mk("alias-b")])
            .with_provider_catalog(providers, pools);
        let a = route_with_seed("alias-a", &table, 0, Some("c1"), 0).unwrap();
        let b = route_with_seed("alias-b", &table, 0, Some("c1"), 0).unwrap();
        assert_eq!(a.provider_id, "c1");
        assert_eq!(b.provider_id, "c1");
    }

    #[test]
    fn empty_pool_returns_pool_error() {
        let providers = vec![catalog_entry("o1", "openai")];
        let pools = auto_kind_pools(&providers);
        let route = Route {
            alias: "claude".into(),
            strategy: RoutingStrategy::Fallback,
            pool_strategy: PoolStrategy::default(),
            targets: vec![RouteTarget {
                provider_id: String::new(),
                model_id: "m".into(),
                provider_kind: "claude-oauth".into(),
                base_url: None,
                weight: 1,
                request_overrides: Default::default(),
                pool_id: None,
                pool_kind: Some("claude-oauth".into()),
            }],
            retry_policy: retry(1),
        };
        let table = RoutingTable::new([route]).with_provider_catalog(providers, pools);
        let err = route_with_seed("claude", &table, 0, None, 0).unwrap_err();
        assert!(matches!(err, RouterError::Pool { .. }), "{err:?}");
    }

    #[test]
    fn attempts_exhausted_returns_error() {
        let table = RoutingTable::new([fixed_route("fast", vec![target("openai", "gpt-4o")])]);
        let err = route_with_seed("fast", &table, 3, None, 0).unwrap_err();
        assert!(matches!(
            err,
            RouterError::AttemptsExhausted {
                attempt_no: 3,
                max_retries: 2,
                ..
            }
        ));
    }

    #[test]
    fn decision_carries_correct_fields() {
        let table = RoutingTable::new([fixed_route("fast", vec![target("openai", "gpt-4o")])]);
        let d = route_with_seed("fast", &table, 1, None, 0).unwrap();
        assert_eq!(d.provider_id, "openai");
        assert_eq!(d.model_id, "gpt-4o");
        assert_eq!(d.provider_kind, "openai");
        assert_eq!(d.attempt_no, 1);
        assert_eq!(d.retry_policy.max_retries, 2);
    }

    #[test]
    fn decision_retry_policy_backoff() {
        let table = RoutingTable::new([fixed_route("fast", vec![target("openai", "gpt-4o")])]);
        let d = route_with_seed("fast", &table, 0, None, 0).unwrap();
        assert_eq!(d.retry_policy.delay_ms(0), 0);
        assert_eq!(d.retry_policy.delay_ms(1), 500);
        assert_eq!(d.retry_policy.delay_ms(2), 1000);
        assert_eq!(d.retry_policy.delay_ms(3), 2000);
    }

    #[test]
    fn decision_should_retry_statuses() {
        let table = RoutingTable::new([fixed_route("fast", vec![target("openai", "gpt-4o")])]);
        let d = route_with_seed("fast", &table, 0, None, 0).unwrap();
        assert!(d.retry_policy.should_retry_status(429));
        assert!(d.retry_policy.should_retry_status(500));
        assert!(!d.retry_policy.should_retry_status(400));
    }
}
