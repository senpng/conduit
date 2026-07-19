//! L3 Router stage: pure function call into conduit-router.

use std::collections::HashSet;

use conduit_ir::error::GatewayError;
use conduit_router::{decision::route_with_options, table::RoutingTable, PoolCursorStore};
use tracing::debug;

use super::context::{PipelineContext, ResolvedProvider};

/// Perform routing for the current attempt and populate ctx.resolved.
pub fn route_request(
    ctx: &mut PipelineContext,
    preferred_provider_id: Option<&str>,
) -> Result<(), GatewayError> {
    route_request_with_skip(ctx, preferred_provider_id, None, None)
}

/// Like [`route_request`] but skips provider ids currently in upstream cooldown.
pub fn route_request_with_skip(
    ctx: &mut PipelineContext,
    preferred_provider_id: Option<&str>,
    skip_provider_ids: Option<&HashSet<String>>,
    pool_cursors: Option<&PoolCursorStore>,
) -> Result<(), GatewayError> {
    let alias = ctx.request.alias.clone();
    let skip_count = skip_provider_ids.map(|s| s.len()).unwrap_or(0);
    debug!(
        request_id = %ctx.request_id,
        alias = %alias,
        attempt_no = ctx.attempt_no,
        preferred = preferred_provider_id.unwrap_or(""),
        skip_count,
        "route decision start"
    );
    let decision = route_with_options(
        &alias,
        &ctx.routing_table,
        ctx.attempt_no,
        preferred_provider_id,
        Some(ctx.routing_seed),
        skip_provider_ids,
        pool_cursors,
    )
    .map_err(|e| {
        debug!(
            request_id = %ctx.request_id,
            alias = %alias,
            attempt_no = ctx.attempt_no,
            error = %e,
            "route decision failed"
        );
        GatewayError::Routing(format!("routing failed for '{}': {}", alias, e))
    })?;

    debug!(
        request_id = %ctx.request_id,
        alias = %alias,
        provider_id = %decision.provider_id,
        provider_kind = %decision.provider_kind,
        model_id = %decision.model_id,
        base_url = decision.base_url.as_deref().unwrap_or(""),
        attempt_no = decision.attempt_no,
        override_keys = decision.request_overrides.len(),
        "route decision ok"
    );

    ctx.resolved = Some(ResolvedProvider {
        provider_id: decision.provider_id.clone(),
        model_id: decision.model_id.clone(),
        provider_kind: decision.provider_kind.clone(),
        base_url: decision.base_url.clone(),
        request_overrides: decision.request_overrides.clone(),
        attempt_no: decision.attempt_no,
    });

    Ok(())
}

/// Check if the current error warrants a retry (advances attempt_no).
pub fn should_retry(
    ctx: &mut PipelineContext,
    table: &RoutingTable,
    error: &conduit_ir::error::ProviderError,
) -> bool {
    let alias = &ctx.request.alias;
    let route = match table.get(alias) {
        Some(r) => r,
        None => {
            debug!(
                request_id = %ctx.request_id,
                alias,
                "should_retry: no route for alias"
            );
            return false;
        }
    };

    let status_hint = error.http_status_hint();
    let should = status_hint
        .map(|s| route.retry_policy.should_retry_status(s))
        .unwrap_or(false)
        && ctx.attempt_no < route.retry_policy.max_retries;

    debug!(
        request_id = %ctx.request_id,
        alias,
        attempt_no = ctx.attempt_no,
        max_retries = route.retry_policy.max_retries,
        status_hint = ?status_hint,
        retryable = should,
        error = %error,
        "should_retry decision"
    );

    if should {
        ctx.attempt_no += 1;
    }
    should
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use conduit_ir::{
        canonical::{CanonicalChatRequest, CanonicalMessage},
        wire_format::WireFormat,
    };
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
            pool_strategy: conduit_router::PoolStrategy::default(),
            targets: vec![
                RouteTarget {
                    provider_id: "a".into(),
                    model_id: "m".into(),
                    provider_kind: "openai".into(),
                    base_url: None,
                    weight: 1,
                    request_overrides: Default::default(),
                    pool_id: None,
                    pool_kind: None,
                },
                RouteTarget {
                    provider_id: "b".into(),
                    model_id: "m".into(),
                    provider_kind: "openai".into(),
                    base_url: None,
                    weight: 1,
                    request_overrides: Default::default(),
                    pool_id: None,
                    pool_kind: None,
                },
            ],
            retry_policy: RetryPolicy {
                max_retries: 1,
                ..Default::default()
            },
        }]))
    }

    #[test]
    fn route_populates_resolved_provider() {
        let table = weighted_table();
        let req = CanonicalChatRequest::new("lb", vec![CanonicalMessage::user("hi")]);
        let mut ctx = PipelineContext::new(req, None, table, WireFormat::OpenaiChat);
        route_request(&mut ctx, None).unwrap();
        let r = ctx.resolved.expect("resolved");
        assert!(r.provider_id == "a" || r.provider_id == "b");
        assert_eq!(r.model_id, "m");
    }
}
