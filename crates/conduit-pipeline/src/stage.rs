//! L3 Router stage: pure function call into conduit-router.

use conduit_ir::error::GatewayError;
use conduit_router::{decision::route, table::RoutingTable};

use super::context::{PipelineContext, ResolvedProvider};

/// Perform routing for the current attempt and populate ctx.resolved.
pub fn route_request(ctx: &mut PipelineContext) -> Result<(), GatewayError> {
    let alias = &ctx.request.alias;
    let decision = route(alias, &ctx.routing_table, ctx.attempt_no)
        .map_err(|e| GatewayError::Routing(format!("routing failed for '{}': {}", alias, e)))?;

    ctx.resolved = Some(ResolvedProvider {
        provider_id: decision.provider_id.clone(),
        model_id: decision.model_id.clone(),
        upstream_key_id: decision.upstream_key_id.clone(),
        provider_kind: decision.provider_kind.clone(),
        base_url: decision.base_url.clone(),
        attempt_no: decision.attempt_no,
    });

    ctx.push_event(conduit_ir::trace::TraceEventKind::RoutingDecided {
        provider_id: decision.provider_id,
        model_id: decision.model_id,
        upstream_key_id: decision.upstream_key_id,
        attempt_no: decision.attempt_no,
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
