//! L3 Router stage: pure function call into conduit-router.

use conduit_ir::error::GatewayError;
use conduit_router::{decision::route_with_seed, table::RoutingTable};

use super::context::{PipelineContext, ResolvedProvider};

/// Perform routing for the current attempt and populate ctx.resolved.
///
/// `preferred_provider_id` is the sticky pin (last success for this key+alias).
/// Applied for multi-target Fallback and Weighted routes.
///
/// Uses [`PipelineContext::routing_seed`] so Weighted retries rebuild the **same**
/// target order (new wall-clock seeds per attempt would re-pick the failed provider).
pub fn route_request(
    ctx: &mut PipelineContext,
    preferred_provider_id: Option<&str>,
) -> Result<(), GatewayError> {
    let alias = &ctx.request.alias;
    let decision = route_with_seed(
        alias,
        &ctx.routing_table,
        ctx.attempt_no,
        preferred_provider_id,
        ctx.routing_seed,
    )
    .map_err(|e| GatewayError::Routing(format!("routing failed for '{}': {}", alias, e)))?;

    ctx.resolved = Some(ResolvedProvider {
        provider_id: decision.provider_id.clone(),
        model_id: decision.model_id.clone(),
        upstream_key_id: decision.upstream_key_id.clone(),
        provider_kind: decision.provider_kind.clone(),
        base_url: decision.base_url.clone(),
        request_overrides: decision.request_overrides.clone(),
        attempt_no: decision.attempt_no,
    });

    ctx.push_event(conduit_ir::trace::TraceEventKind::RoutingDecided {
        provider_id: decision.provider_id,
        model_id: decision.model_id,
        upstream_key_id: decision.upstream_key_id,
        attempt_no: decision.attempt_no,
        request_overrides: decision.request_overrides,
        attempt_loss: None,
    });

    Ok(())
}

/// Check if the current error warrants a retry (advances attempt_no).
/// Returns true if caller should loop and retry.
pub fn should_retry(
    ctx: &mut PipelineContext,
    table: &RoutingTable,
    error: &conduit_ir::error::ProviderError,
) -> bool {
    let alias = &ctx.request.alias;
    let route = match table.get(alias) {
        Some(r) => r,
        None => return false,
    };

    let should = error
        .http_status_hint()
        .map(|s| route.retry_policy.should_retry_status(s))
        .unwrap_or(false)
        && ctx.attempt_no < route.retry_policy.max_retries;

    if should {
        ctx.attempt_no += 1;
    }
    should
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use conduit_ir::canonical::{CanonicalChatRequest, CanonicalMessage, Role};
    use conduit_router::{
        policy::RetryPolicy,
        table::{Route, RouteTarget, RoutingStrategy, RoutingTable},
    };

    use super::*;
    use crate::context::PipelineContext;

    fn weighted_table() -> Arc<RoutingTable> {
        Arc::new(RoutingTable::new([Route {
            alias: "lb".into(),
            strategy: RoutingStrategy::Weighted,
            targets: vec![
                RouteTarget {
                    provider_id: "a".into(),
                    model_id: "m".into(),
                    upstream_key_id: "ka".into(),
                    provider_kind: "openai".into(),
                    base_url: None,
                    weight: 1,
                    request_overrides: Default::default(),
                },
                RouteTarget {
                    provider_id: "b".into(),
                    model_id: "m".into(),
                    upstream_key_id: "kb".into(),
                    provider_kind: "openai".into(),
                    base_url: None,
                    weight: 1,
                    request_overrides: Default::default(),
                },
            ],
            retry_policy: RetryPolicy {
                max_retries: 2,
                ..Default::default()
            },
        }]))
    }

    fn sample_req() -> CanonicalChatRequest {
        CanonicalChatRequest::new(
            "lb",
            vec![CanonicalMessage {
                role: Role::User,
                content: vec![],
                name: None,
            }],
        )
    }

    /// Pipeline re-entry must reuse ctx.routing_seed so Weighted attempt 1
    /// cannot reselect attempt 0's provider (equal weights + unlucky new seeds).
    #[test]
    fn weighted_retry_via_route_request_keeps_seed_and_changes_provider() {
        let table = weighted_table();
        let mut ctx = PipelineContext::new(sample_req(), Some("dk".into()), table);
        // Force a known seed so attempt 0 is deterministic.
        ctx.routing_seed = 0; // equal weights → index 0 → "a"

        route_request(&mut ctx, None).unwrap();
        let first = ctx.resolved.as_ref().unwrap().provider_id.clone();
        assert_eq!(first, "a");

        // Simulate retry without re-seeding (production should_retry only bumps attempt_no).
        ctx.attempt_no = 1;
        // Poison seed would reselect "a" if route() re-drew from wall clock with seed=2;
        // request-scoped seed=0 must still pick remaining "b".
        route_request(&mut ctx, None).unwrap();
        let second = ctx.resolved.as_ref().unwrap().provider_id.clone();
        assert_eq!(second, "b");
        assert_ne!(first, second);
    }

    #[test]
    fn routing_event_records_selected_target_request_overrides() {
        let table = Arc::new(RoutingTable::new([Route {
            alias: "lb".into(),
            strategy: RoutingStrategy::Fixed,
            targets: vec![RouteTarget {
                provider_id: "a".into(),
                model_id: "m".into(),
                upstream_key_id: "ka".into(),
                provider_kind: "openai".into(),
                base_url: None,
                weight: 1,
                request_overrides: serde_json::Map::from_iter([(
                    "service_tier".into(),
                    serde_json::json!("flex"),
                )]),
            }],
            retry_policy: RetryPolicy::default(),
        }]));
        let mut ctx = PipelineContext::new(sample_req(), Some("dk".into()), table);

        route_request(&mut ctx, None).unwrap();

        match &ctx.events[0] {
            conduit_ir::trace::TraceEventKind::RoutingDecided {
                request_overrides,
                ..
            } => assert_eq!(request_overrides["service_tier"], "flex"),
            _ => panic!("expected routing_decided event"),
        }
    }
}
