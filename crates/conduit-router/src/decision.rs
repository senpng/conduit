use std::time::{SystemTime, UNIX_EPOCH};

use thiserror::Error;

use crate::{
    policy::RetryPolicy,
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
}

// ---------------------------------------------------------------------------
// RoutingDecision
// ---------------------------------------------------------------------------

/// The output of a successful routing call.
#[derive(Debug, Clone)]
pub struct RoutingDecision {
    pub provider_id: String,
    pub model_id: String,
    pub upstream_key_id: String,
    pub provider_kind: String,
    pub base_url: Option<String>,
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
/// Sticky affinity: when `preferred_provider_id` is set and still on the route,
/// it is preferred for attempt 0 (and reorders the retry chain) for
/// [`Fallback`] and [`Weighted`].
///
/// | Strategy   | Behaviour |
/// |------------|-----------|
/// | `Fixed`    | Always `targets[0]`. |
/// | `Fallback` | Sticky-aware ordered failover. |
/// | `Weighted` | Sticky pin if present; else weighted pick on attempt 0; retries remaining. |
pub fn route(
    alias: &str,
    table: &RoutingTable,
    attempt_no: u32,
    preferred_provider_id: Option<&str>,
) -> Result<RoutingDecision, RouterError> {
    let seed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    route_with_seed(alias, table, attempt_no, preferred_provider_id, seed)
}

/// Same as [`route`] but with an explicit seed for weighted load-balancing.
///
/// Pure with respect to inputs: same `(alias, table, attempt_no, preferred, seed)`
/// always yields the same decision.
pub fn route_with_seed(
    alias: &str,
    table: &RoutingTable,
    attempt_no: u32,
    preferred_provider_id: Option<&str>,
    seed: u64,
) -> Result<RoutingDecision, RouterError> {
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

    let target = match r.strategy {
        RoutingStrategy::Fixed => &r.targets[0],
        RoutingStrategy::Fallback => {
            let order = sticky_target_order(&r.targets, preferred_provider_id);
            let idx = (attempt_no as usize).min(order.len() - 1);
            order[idx]
        }
        RoutingStrategy::Weighted => {
            let order = weighted_target_order(&r.targets, preferred_provider_id, seed);
            let idx = (attempt_no as usize).min(order.len() - 1);
            order[idx]
        }
    };

    Ok(RoutingDecision {
        provider_id: target.provider_id.clone(),
        model_id: target.model_id.clone(),
        upstream_key_id: target.upstream_key_id.clone(),
        provider_kind: target.provider_kind.clone(),
        base_url: target.base_url.clone(),
        attempt_no,
        retry_policy: r.retry_policy.clone(),
    })
}

/// Preferred provider first (if still listed), then remaining targets in table order.
///
/// Shared by sticky fallback / affinity and as the pin path for weighted LB.
pub fn sticky_target_order<'a>(
    targets: &'a [RouteTarget],
    preferred_provider_id: Option<&str>,
) -> Vec<&'a RouteTarget> {
    if let Some(pref) = preferred_provider_id.filter(|s| !s.is_empty()) {
        if targets.iter().any(|t| t.provider_id == pref) {
            let mut out = Vec::with_capacity(targets.len());
            if let Some(t) = targets.iter().find(|t| t.provider_id == pref) {
                out.push(t);
            }
            for t in targets {
                if t.provider_id != pref {
                    out.push(t);
                }
            }
            return out;
        }
    }
    targets.iter().collect()
}

/// Build attempt order for weighted LB: sticky pin wins; else weight-pick first.
pub fn weighted_target_order<'a>(
    targets: &'a [RouteTarget],
    preferred_provider_id: Option<&str>,
    seed: u64,
) -> Vec<&'a RouteTarget> {
    if preferred_provider_id
        .filter(|s| !s.is_empty())
        .is_some_and(|p| targets.iter().any(|t| t.provider_id == p))
    {
        return sticky_target_order(targets, preferred_provider_id);
    }

    let first_idx = pick_weighted_index(targets, seed);
    let first = &targets[first_idx];
    let mut out = Vec::with_capacity(targets.len());
    out.push(first);
    for (i, t) in targets.iter().enumerate() {
        if i != first_idx {
            out.push(t);
        }
    }
    out
}

/// Weighted index among targets. Zero-weight targets are skipped; if all zero,
/// falls back to `seed % len`.
pub fn pick_weighted_index(targets: &[RouteTarget], seed: u64) -> usize {
    if targets.is_empty() {
        return 0;
    }
    let weights: Vec<u32> = targets.iter().map(|t| t.weight).collect();
    let total: u64 = weights.iter().map(|&w| u64::from(w)).sum();
    if total == 0 {
        return (seed as usize) % targets.len();
    }
    let mut r = seed % total;
    for (i, &w) in weights.iter().enumerate() {
        let w = u64::from(w);
        if r < w {
            return i;
        }
        r -= w;
    }
    targets.len() - 1
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
        table::{Route, RouteTarget, RoutingStrategy, RoutingTable},
    };

    fn target(provider: &str, model: &str) -> RouteTarget {
        target_w(provider, model, 1)
    }

    fn target_w(provider: &str, model: &str, weight: u32) -> RouteTarget {
        RouteTarget {
            provider_id: provider.into(),
            model_id: model.into(),
            upstream_key_id: format!("key_{provider}"),
            provider_kind: provider.into(),
            base_url: None,
            weight,
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
            targets,
            retry_policy: retry(2),
        }
    }

    fn fallback_route(alias: &str, targets: Vec<RouteTarget>) -> Route {
        Route {
            alias: alias.into(),
            strategy: RoutingStrategy::Fallback,
            targets,
            retry_policy: retry(3),
        }
    }

    fn weighted_route(alias: &str, targets: Vec<RouteTarget>) -> Route {
        Route {
            alias: alias.into(),
            strategy: RoutingStrategy::Weighted,
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

        // Simulate success on p2
        store.remember("key-a", "fb", "p2");
        store.remember("key-a", "lb", "p2");

        let pref_fb = store.preferred("key-a", "fb");
        assert_eq!(
            route_with_seed("fb", &table, 0, pref_fb.as_deref(), 0)
                .unwrap()
                .provider_id,
            "p2"
        );

        let pref_lb = store.preferred("key-a", "lb");
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
            targets: vec![],
            retry_policy: retry(2),
        }]);
        let err = route_with_seed("empty", &table, 0, None, 0).unwrap_err();
        assert!(matches!(err, RouterError::NoTargets { .. }));
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
        assert_eq!(d.upstream_key_id, "key_openai");
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
