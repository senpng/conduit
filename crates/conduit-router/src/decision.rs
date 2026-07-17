use thiserror::Error;

use crate::{
    policy::RetryPolicy,
    table::{RoutingStrategy, RoutingTable},
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
// route — pure, deterministic function
// ---------------------------------------------------------------------------

/// Select a target for the given `alias` and `attempt_no`.
///
/// This function is **pure** — it has no I/O, no locks, and produces the same
/// output for the same inputs. It is 100% unit-testable.
///
/// # Strategy semantics
///
/// | Strategy  | Behaviour |
/// |-----------|-----------|
/// | `Fixed`   | Always returns `targets[0]`, regardless of `attempt_no`. |
/// | `Fallback`| Returns `targets[attempt_no]`, clamping to the last entry. |
///
/// # Errors
///
/// Returns `RouterError::UnknownAlias` when the alias does not exist in the
/// table, `RouterError::NoTargets` when the route has no entries, and
/// `RouterError::AttemptsExhausted` when `attempt_no > max_retries`.
pub fn route(
    alias: &str,
    table: &RoutingTable,
    attempt_no: u32,
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
            let idx = (attempt_no as usize).min(r.targets.len() - 1);
            &r.targets[idx]
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

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        policy::RetryPolicy,
        table::{Route, RouteTarget, RoutingStrategy, RoutingTable},
    };

    fn target(provider: &str, model: &str) -> RouteTarget {
        RouteTarget {
            provider_id: provider.into(),
            model_id: model.into(),
            upstream_key_id: format!("key_{provider}"),
            provider_kind: provider.into(),
            base_url: None,
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

    // -----------------------------------------------------------------------
    // Fixed strategy
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
            let d = route("fast", &table, attempt).unwrap();
            assert_eq!(
                d.provider_id, "openai",
                "attempt {attempt} should use first target"
            );
            assert_eq!(d.model_id, "gpt-4o-mini");
        }
    }

    #[test]
    fn fixed_single_target() {
        let table = RoutingTable::new([fixed_route("solo", vec![target("openai", "gpt-4o")])]);
        let d = route("solo", &table, 0).unwrap();
        assert_eq!(d.provider_id, "openai");
    }

    // -----------------------------------------------------------------------
    // Fallback strategy
    // -----------------------------------------------------------------------

    #[test]
    fn fallback_advances_with_attempt_no() {
        let table = RoutingTable::new([fallback_route(
            "smart",
            vec![
                target("openai", "gpt-4o"),
                target("anthropic", "claude-sonnet"),
                target("gemini", "gemini-2.0-flash"),
            ],
        )]);

        let d0 = route("smart", &table, 0).unwrap();
        assert_eq!(d0.provider_id, "openai");

        let d1 = route("smart", &table, 1).unwrap();
        assert_eq!(d1.provider_id, "anthropic");

        let d2 = route("smart", &table, 2).unwrap();
        assert_eq!(d2.provider_id, "gemini");
    }

    #[test]
    fn fallback_clamps_to_last_target() {
        let table = RoutingTable::new([fallback_route(
            "smart",
            vec![
                target("openai", "gpt-4o"),
                target("anthropic", "claude-sonnet"),
            ],
        )]);

        // attempt_no 2 and 3 should both land on the last target (index 1)
        let d2 = route("smart", &table, 2).unwrap();
        assert_eq!(d2.provider_id, "anthropic");

        let d3 = route("smart", &table, 3).unwrap();
        assert_eq!(d3.provider_id, "anthropic");
    }

    // -----------------------------------------------------------------------
    // Error cases
    // -----------------------------------------------------------------------

    #[test]
    fn unknown_alias_returns_error() {
        let table = RoutingTable::new([fixed_route("fast", vec![target("openai", "gpt-4o")])]);
        let err = route("nonexistent", &table, 0).unwrap_err();
        assert!(matches!(err, RouterError::UnknownAlias { .. }));
        assert!(err.to_string().contains("nonexistent"));
    }

    #[test]
    fn empty_targets_returns_error() {
        let table = RoutingTable::new([Route {
            alias: "empty".into(),
            strategy: RoutingStrategy::Fixed,
            targets: vec![],
            retry_policy: retry(2),
        }]);
        let err = route("empty", &table, 0).unwrap_err();
        assert!(matches!(err, RouterError::NoTargets { .. }));
    }

    #[test]
    fn attempts_exhausted_returns_error() {
        let table = RoutingTable::new([fixed_route("fast", vec![target("openai", "gpt-4o")])]);
        // max_retries is 2, so attempt 3 should fail
        let err = route("fast", &table, 3).unwrap_err();
        assert!(
            matches!(
                err,
                RouterError::AttemptsExhausted {
                    attempt_no: 3,
                    max_retries: 2,
                    ..
                }
            ),
            "got: {err:?}"
        );
    }

    // -----------------------------------------------------------------------
    // Decision fields
    // -----------------------------------------------------------------------

    #[test]
    fn decision_carries_correct_fields() {
        let table = RoutingTable::new([fixed_route("fast", vec![target("openai", "gpt-4o")])]);
        let d = route("fast", &table, 1).unwrap();
        assert_eq!(d.provider_id, "openai");
        assert_eq!(d.model_id, "gpt-4o");
        assert_eq!(d.upstream_key_id, "key_openai");
        assert_eq!(d.provider_kind, "openai");
        assert_eq!(d.attempt_no, 1);
        assert_eq!(d.retry_policy.max_retries, 2);
    }

    // -----------------------------------------------------------------------
    // Retry policy backoff (via decision)
    // -----------------------------------------------------------------------

    #[test]
    fn decision_retry_policy_backoff() {
        let table = RoutingTable::new([fixed_route("fast", vec![target("openai", "gpt-4o")])]);
        let d = route("fast", &table, 0).unwrap();
        assert_eq!(d.retry_policy.delay_ms(0), 0);
        assert_eq!(d.retry_policy.delay_ms(1), 500);
        assert_eq!(d.retry_policy.delay_ms(2), 1000);
        assert_eq!(d.retry_policy.delay_ms(3), 2000); // capped at 4×
        assert_eq!(d.retry_policy.delay_ms(10), 2000); // still capped
    }

    #[test]
    fn decision_should_retry_statuses() {
        let table = RoutingTable::new([fixed_route("fast", vec![target("openai", "gpt-4o")])]);
        let d = route("fast", &table, 0).unwrap();
        assert!(d.retry_policy.should_retry_status(429));
        assert!(d.retry_policy.should_retry_status(500));
        assert!(d.retry_policy.should_retry_status(503));
        assert!(!d.retry_policy.should_retry_status(400));
        assert!(!d.retry_policy.should_retry_status(200));
    }
}
