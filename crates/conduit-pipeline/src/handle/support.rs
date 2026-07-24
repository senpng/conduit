//! Shared pipeline helpers: session, affinity, usage ledger, observability.

use conduit_ir::{
    canonical::CanonicalChatRequest,
    error::{GatewayError, ProviderError},
};
use conduit_router::{extract_session_id, is_usage_limit_body, parse_cooldown_duration};
use tracing::{debug, warn};

use super::super::{
    context::{PipelineContext, ResolvedProvider},
    ingress,
};
use super::{PipelineHandle, UpstreamAuth};

impl PipelineHandle {
    /// L2 checks with typed error metadata for HTTP status mapping.
    pub(crate) async fn run_ingress_checks(
        &self,
        ctx: &mut PipelineContext,
        downstream_bearer: Option<String>,
    ) -> Result<(), (&'static str, u16, GatewayError)> {
        let raw = ingress::require_bearer(downstream_bearer.as_deref()).map_err(|e| {
            (
                "Unauthorized",
                401u16,
                e,
            )
        })?;
        let policy_opt = (self.deps.key_policy_fn)(raw.to_string())
            .await
            .map_err(|e| ("Unauthorized", 401u16, e))?;
        let policy = ingress::accept_policy(raw, policy_opt).map_err(|e| {
            (
                "Unauthorized",
                401u16,
                e,
            )
        })?;
        debug!(
            request_id = %ctx.request_id,
            key_id = %policy.key_id,
            rate_limit_rpm = ?policy.rate_limit_rpm,
            whitelist_len = policy.model_whitelist.len(),
            "pipeline key policy accepted"
        );
        policy
            .check_model_allowed(&ctx.request.alias)
            .map_err(|e| ("ModelNotAllowed", 403u16, e))?;

        let quota_req =
            ingress::build_quota_check(&policy, &ctx.request.alias, ctx.request_id.clone());
        self.deps.quota.check(&quota_req).await.map_err(|e| {
            let status = match &e {
                conduit_ir::error::QuotaError::RateLimitExceeded { .. } => 429u16,
                _ => 403u16,
            };
            ("QuotaError", status, GatewayError::from(e))
        })?;
        debug!(
            request_id = %ctx.request_id,
            key_id = %policy.key_id,
            alias = %ctx.request.alias,
            "pipeline quota check passed"
        );

        ctx.downstream_key_id = Some(policy.key_id.clone());
        Ok(())
    }

    /// Sink for successful/error response rate-limit headers → quota snapshot store.
    pub(crate) fn rate_limit_sink(&self) -> conduit_upstream::RateLimitHeaderSink {
        let store = self.deps.quota_snapshots.clone();
        std::sync::Arc::new(move |provider_id: &str, headers: Vec<(String, String)>| {
            store.record_headers(provider_id, headers);
        })
    }

    /// Mark provider cooling on 429 / usage_limit so multi-target routes skip it.
    pub(crate) fn note_upstream_error(&self, resolved: &ResolvedProvider, err: &ProviderError) {
        let ProviderError::RateLimited(body) = err else {
            return;
        };
        let duration = parse_cooldown_duration(body);
        let reason = if is_usage_limit_body(body) {
            "usage_limit"
        } else {
            "rate_limited"
        };
        tracing::warn!(
            provider_id = %resolved.provider_id,
            secs = duration.as_secs(),
            reason,
            "upstream cooldown: marking provider"
        );
        self.deps
            .cooldown
            .mark(&resolved.provider_id, duration, reason, 429);
        self.deps
            .quota_snapshots
            .record_error_body(&resolved.provider_id, body);
    }

    pub(crate) async fn resolve_auth(&self, key_id: &str) -> Result<UpstreamAuth, GatewayError> {
        (self.deps.secret_fn)(key_id.to_string()).await
    }

    /// Session-scoped preferred provider when a session id is present.
    pub(crate) fn session_preferred(&self, ctx: &PipelineContext) -> Option<String> {
        let sid = ctx.session_id.as_deref()?.trim();
        if sid.is_empty() {
            return None;
        }
        self.deps.affinity.preferred(sid, &ctx.request.alias)
    }

    /// Remember successful provider for this **session** (not downstream key).
    ///
    /// Applies to multi-target fallback/weighted and any pool route. No-ops
    /// when no session id was extracted.
    pub(crate) fn remember_session_affinity(&self, ctx: &PipelineContext, resolved: &ResolvedProvider) {
        let Some(sid) = ctx.session_id.as_deref().map(str::trim).filter(|s| !s.is_empty()) else {
            return;
        };
        let alias = ctx.request.alias.as_str();
        let route = ctx.routing_table.get(alias);
        let uses_affinity = route
            .map(|r| {
                let has_pool = r.targets.iter().any(|t| t.is_pool_target());
                if has_pool {
                    return true;
                }
                matches!(
                    r.strategy,
                    conduit_router::table::RoutingStrategy::Fallback
                        | conduit_router::table::RoutingStrategy::Weighted
                ) && r.targets.len() > 1
            })
            .unwrap_or(false);
        if !uses_affinity {
            return;
        }
        debug!(
            request_id = %ctx.request_id,
            session_id = %sid,
            alias,
            provider_id = %resolved.provider_id,
            "pipeline session affinity remembered"
        );
        self.deps
            .affinity
            .remember(sid, alias, &resolved.provider_id);
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn record_usage(
        &self,
        ctx: &PipelineContext,
        resolved: &ResolvedProvider,
        cost_usd: f64,
        stream: bool,
        status: &str,
        error_class: Option<String>,
        http_status: Option<u16>,
        finish_reason: Option<String>,
        duration_ms: Option<u64>,
        ttfb_ms: Option<u64>,
        attempts: &[conduit_quota::QuotaAttemptRecord],
    ) {
        let key_id = ctx
            .downstream_key_id
            .clone()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "_anonymous".into());
        let preferred = self.session_preferred(ctx);
        let route_meta = route_observability(ctx, resolved, preferred.as_deref());
        let attempt_no = resolved.attempt_no;
        let attempt_count = attempts
            .iter()
            .map(|a| a.attempt_no + 1)
            .max()
            .unwrap_or(attempt_no + 1)
            .max(1);
        let req = conduit_quota::QuotaRecordRequest {
            request_id: ctx.request_id.clone(),
            downstream_key_id: key_id,
            alias: Some(ctx.request.alias.clone()),
            provider_id: Some(resolved.provider_id.clone()),
            provider_kind: Some(resolved.provider_kind.clone()),
            model_id: Some(resolved.model_id.clone()),
            prompt_tokens: ctx.usage.prompt_tokens,
            completion_tokens: ctx.usage.completion_tokens,
            total_tokens: ctx.usage.total_tokens,
            reasoning_tokens: ctx.usage.reasoning_tokens,
            cache_read_tokens: ctx.usage.cache_read_tokens,
            cache_write_tokens: ctx.usage.cache_write_tokens,
            cost_usd,
            stream,
            status: status.into(),
            error_class,
            http_status,
            finish_reason,
            duration_ms,
            ttfb_ms,
            route_strategy: route_meta.route_strategy,
            attempt_no,
            attempt_count,
            session_id: ctx.session_id.clone(),
            affinity_hit: route_meta.affinity_hit,
            pool_id: route_meta.pool_id,
            selected_reason: route_meta.selected_reason,
            loss_count: ctx.loss_report.len() as u32,
            wire_format: ctx
                .ingress_wire
                .as_ref()
                .map(|w| w.format.as_str().to_string()),
            attempts: attempts.to_vec(),
        };
        if let Err(e) = self.deps.quota.record(&req).await {
            warn!(error = %e, request_id = %ctx.request_id, "usage record failed");
        }
    }
}

/// Resolve session id from headers and request metadata (for affinity).
///
/// Passes headers and body metadata **together** so Claude `metadata.user_id`
/// can outrank generic session headers (CLIProxyAPI affinity parity).
///
/// Public for integration tests of the live header → pin path.
pub fn resolve_session_id(
    headers: &[(String, String)],
    request: &CanonicalChatRequest,
) -> Option<String> {
    // Fold RequestMeta into a JSON object for the shared extractor.
    let mut map = serde_json::Map::new();
    if let Some(ref user) = request.meta.user {
        let mut meta = serde_json::Map::new();
        meta.insert("user_id".into(), serde_json::Value::String(user.clone()));
        map.insert("metadata".into(), serde_json::Value::Object(meta));
    }
    for (k, v) in &request.meta.extra {
        map.insert(k.clone(), v.clone());
    }
    let body = if map.is_empty() {
        None
    } else {
        Some(serde_json::Value::Object(map))
    };
    extract_session_id(headers, body.as_ref())
}


/// Emit a structured warn when codec translation degraded any fields.
/// Ledger only stores `loss_count`; field detail lives here for operators.
pub(crate) fn log_codec_losses(ctx: &PipelineContext) {
    if ctx.loss_report.is_empty() {
        return;
    }
    warn!(
        request_id = %ctx.request_id,
        alias = %ctx.request.alias,
        wire = %ctx
            .ingress_wire
            .as_ref()
            .map(|w| w.format.as_str())
            .unwrap_or(""),
        loss_count = ctx.loss_report.len(),
        loss_fields = %ctx.loss_report.field_names(),
        loss_summary = %ctx.loss_report.summary(),
        "codec translation losses"
    );
}

pub(crate) struct RouteObs {
    pub(crate) route_strategy: Option<String>,
    pub(crate) affinity_hit: Option<bool>,
    pub(crate) pool_id: Option<String>,
    pub(crate) selected_reason: Option<String>,
}

pub(crate) fn route_observability(
    ctx: &PipelineContext,
    resolved: &ResolvedProvider,
    session_preferred: Option<&str>,
) -> RouteObs {
    let route = ctx.routing_table.get(&ctx.request.alias);
    let has_pool = route
        .map(|r| r.targets.iter().any(|t| t.is_pool_target()))
        .unwrap_or(false);
    let pool_id = route.and_then(|r| {
        r.targets
            .iter()
            .find_map(|t| t.pool_id.clone().or_else(|| t.pool_kind.clone()))
    });
    let route_strategy = route.map(|r| {
        if has_pool {
            "pool".to_string()
        } else {
            match r.strategy {
                conduit_router::table::RoutingStrategy::Fixed => "fixed".into(),
                conduit_router::table::RoutingStrategy::Fallback => "fallback".into(),
                conduit_router::table::RoutingStrategy::Weighted => "weighted".into(),
            }
        }
    });

    let pin_match = session_preferred
        .map(|p| p == resolved.provider_id.as_str())
        .unwrap_or(false);
    let affinity_hit = if session_preferred.is_some() && resolved.attempt_no == 0 {
        Some(pin_match)
    } else if session_preferred.is_some() {
        Some(false)
    } else {
        None
    };

    let selected_reason = if resolved.attempt_no > 0 {
        Some("retry".into())
    } else if pin_match {
        Some("session_pin".into())
    } else if has_pool {
        Some("pool".into())
    } else {
        route_strategy.clone()
    };

    RouteObs {
        route_strategy,
        affinity_hit,
        pool_id,
        selected_reason,
    }
}

pub(crate) fn attempt_record(
    resolved: &ResolvedProvider,
    status: &str,
    error_class: Option<String>,
    http_status: Option<u16>,
    duration_ms: Option<u64>,
    ttfb_ms: Option<u64>,
) -> conduit_quota::QuotaAttemptRecord {
    conduit_quota::QuotaAttemptRecord {
        attempt_no: resolved.attempt_no,
        provider_id: Some(resolved.provider_id.clone()),
        provider_kind: Some(resolved.provider_kind.clone()),
        model_id: Some(resolved.model_id.clone()),
        status: status.into(),
        error_class,
        http_status,
        duration_ms,
        ttfb_ms,
        reason: Some(if resolved.attempt_no == 0 {
            "initial".into()
        } else {
            "retry".into()
        }),
    }
}

pub(crate) fn elapsed_ms(started: chrono::DateTime<chrono::Utc>) -> u64 {
    (chrono::Utc::now() - started).num_milliseconds().max(0) as u64
}

pub(crate) fn request_for_upstream(
    request: &CanonicalChatRequest,
    resolved: &ResolvedProvider,
) -> CanonicalChatRequest {
    let mut req = request.clone();
    if !resolved.model_id.is_empty() {
        req.alias = resolved.model_id.clone();
    }
    req
}

