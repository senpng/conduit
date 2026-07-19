//! PipelineHandle: main entry point for running requests through the L2-L7 pipeline.

use std::{future::Future, pin::Pin, sync::Arc};

use conduit_ir::{
    canonical::{CanonicalChatRequest, CanonicalChatResponse, CanonicalChunk},
    error::{GatewayError, ProviderError},
    wire_format::WireFormat,
};
use conduit_router::{
    extract_session_id, is_usage_limit_body, parse_cooldown_duration, table::RoutingTable,
    AffinityStore, PoolCursorStore, ProviderCooldownStore, UpstreamQuotaStore,
};
use futures::stream::BoxStream;
use tracing::{debug, info, warn};

use super::{
    context::{IngressWire, PipelineContext, ResolvedProvider},
    egress,
    ingress::{self, KeyPolicy},
    provider::{
        dispatch_non_stream, dispatch_responses_compact, dispatch_stream, UpstreamAuth,
    },
    stage::{route_request_with_skip, should_retry},
    stream_probe::{
        finish_reason_str, provider_error_class, StreamRecordMeta, UsageTrackingStream,
    },
};

// ── Async dependency types ────────────────────────────────────────────────────

pub type BoxFut<T> = Pin<Box<dyn Future<Output = T> + Send + 'static>>;

pub type KeyPolicyFn =
    Arc<dyn Fn(String) -> BoxFut<Result<Option<KeyPolicy>, GatewayError>> + Send + Sync>;

pub type AuthFn = Arc<dyn Fn(String) -> BoxFut<Result<UpstreamAuth, GatewayError>> + Send + Sync>;

pub use super::stream_probe::PricingFn;

pub struct PipelineDeps {
    pub routing_table: Arc<arc_swap::ArcSwap<RoutingTable>>,
    pub secret_fn: AuthFn,
    pub pricing_fn: PricingFn,
    pub quota: Arc<dyn conduit_quota::engine::QuotaEngine>,
    pub key_policy_fn: KeyPolicyFn,
    /// Session → provider affinity (default base layer for multi-target / pool).
    pub affinity: Arc<AffinityStore>,
    /// Round-robin cursors for pool routes (per alias).
    pub pool_cursors: Arc<PoolCursorStore>,
    /// Upstream provider cooldown after 429 / usage_limit (CLIProxyAPI parity).
    pub cooldown: Arc<ProviderCooldownStore>,
    /// Last-seen rate-limit / quota signals from upstream responses.
    pub quota_snapshots: Arc<UpstreamQuotaStore>,
}

pub enum PipelineResult {
    Complete(CanonicalChatResponse),
    Streaming(BoxStream<'static, Result<CanonicalChunk, ProviderError>>),
}

pub struct PipelineHandle {
    deps: Arc<PipelineDeps>,
}

impl PipelineHandle {
    pub fn new(deps: Arc<PipelineDeps>) -> Self {
        Self { deps }
    }

    pub fn deps(&self) -> &PipelineDeps {
        &self.deps
    }

    pub async fn run(
        &self,
        request: CanonicalChatRequest,
        downstream_bearer: Option<String>,
        client_headers: Vec<(String, String)>,
        ingress_wire: IngressWire,
    ) -> Result<PipelineResult, GatewayError> {
        let table_snap = self.deps.routing_table.load_full();
        let stream = request.stream;
        let wire_format = ingress_wire.format;
        let session_id = resolve_session_id(&client_headers, &request);
        let message_count = request.messages.len();
        let mut ctx = PipelineContext::new(request, None, table_snap, wire_format)
            .with_ingress_wire(ingress_wire)
            .with_client_headers(client_headers.clone());
        ctx.session_id = session_id.clone();

        debug!(
            request_id = %ctx.request_id,
            client_request_id = %ctx.request.id,
            alias = %ctx.request.alias,
            stream,
            wire = %wire_format.as_str(),
            message_count,
            session_id = session_id.as_deref().unwrap_or(""),
            has_bearer = downstream_bearer.as_ref().is_some_and(|s| !s.trim().is_empty()),
            client_header_count = client_headers.len(),
            "pipeline request start"
        );

        if let Err((error_type, http_status, err)) =
            self.run_ingress_checks(&mut ctx, downstream_bearer).await
        {
            debug!(
                request_id = %ctx.request_id,
                alias = %ctx.request.alias,
                error_type,
                http_status,
                error = %err,
                "pipeline ingress rejected"
            );
            return Err(err);
        }

        let preferred = self.session_preferred(&ctx);
        let skip = self.deps.cooldown.cooling_ids();
        if !skip.is_empty() {
            debug!(
                request_id = %ctx.request_id,
                cooling = ?skip,
                preferred = preferred.as_deref().unwrap_or(""),
                "pipeline routing with cooldown skip set"
            );
        }
        if let Err(e) = route_request_with_skip(
            &mut ctx,
            preferred.as_deref(),
            Some(&skip),
            Some(self.deps.pool_cursors.as_ref()),
        ) {
            debug!(
                request_id = %ctx.request_id,
                alias = %ctx.request.alias,
                error = %e,
                "pipeline routing failed"
            );
            return Err(e);
        }

        let resolved = ctx.resolved.as_ref().unwrap().clone();
        debug!(
            request_id = %ctx.request_id,
            alias = %ctx.request.alias,
            provider_id = %resolved.provider_id,
            provider_kind = %resolved.provider_kind,
            model_id = %resolved.model_id,
            base_url = resolved.base_url.as_deref().unwrap_or(""),
            attempt_no = resolved.attempt_no,
            preferred = preferred.as_deref().unwrap_or(""),
            "pipeline route resolved"
        );

        let mut auth = match self.resolve_auth(&resolved.provider_id).await {
            Ok(a) => {
                debug!(
                    request_id = %ctx.request_id,
                    provider_id = %resolved.provider_id,
                    extra_header_count = a.extra_headers.len(),
                    using_api = a.using_api,
                    "pipeline credential resolved"
                );
                a
            }
            Err(e) => {
                debug!(
                    request_id = %ctx.request_id,
                    provider_id = %resolved.provider_id,
                    error = %e,
                    "pipeline credential resolve failed"
                );
                return Err(e);
            }
        };
        auth.client_headers = client_headers;

        if stream {
            self.run_stream(ctx, resolved, auth).await
        } else {
            self.run_non_stream(ctx, resolved, auth).await
        }
    }

    /// `POST /v1/responses/compact` — context compaction (Codex).
    ///
    /// Uses the same ingress/routing/auth path as chat, but forwards a raw
    /// Responses body (preserving `compaction_trigger`) to the provider compact
    /// endpoint instead of IR chat encode/decode.
    pub async fn run_compact(
        &self,
        alias: String,
        body: serde_json::Value,
        downstream_bearer: Option<String>,
        client_headers: Vec<(String, String)>,
    ) -> Result<serde_json::Value, GatewayError> {
        use conduit_ir::canonical::CanonicalChatRequest;

        let table_snap = self.deps.routing_table.load_full();
        // Minimal IR request for routing / quota / session only.
        let mut request = CanonicalChatRequest::new(alias, vec![]);
        request.stream = false;
        let session_id = resolve_session_id(&client_headers, &request);
        let mut ctx = PipelineContext::new(
            request,
            None,
            table_snap,
            WireFormat::OpenaiResponses,
        )
        .with_ingress_wire(IngressWire {
            format: WireFormat::OpenaiResponses,
        })
        .with_client_headers(client_headers.clone());
        ctx.session_id = session_id;

        if let Err((_t, _s, err)) = self.run_ingress_checks(&mut ctx, downstream_bearer).await {
            return Err(err);
        }

        let preferred = self.session_preferred(&ctx);
        let skip = self.deps.cooldown.cooling_ids();
        route_request_with_skip(
            &mut ctx,
            preferred.as_deref(),
            Some(&skip),
            Some(self.deps.pool_cursors.as_ref()),
        )?;

        let resolved = ctx.resolved.as_ref().unwrap().clone();
        let mut auth = self.resolve_auth(&resolved.provider_id).await?;
        auth.client_headers = client_headers;

        let attempt_started = chrono::Utc::now();
        debug!(
            request_id = %ctx.request_id,
            provider_id = %resolved.provider_id,
            provider_kind = %resolved.provider_kind,
            model_id = %resolved.model_id,
            "pipeline compact attempt"
        );

        match dispatch_responses_compact(
            &resolved,
            body,
            &auth,
            Some(self.rate_limit_sink()),
        )
        .await
        {
            Ok(resp) => {
                // Best-effort usage from compact response.
                if let Some(usage) = resp.get("usage") {
                    let prompt = usage
                        .get("input_tokens")
                        .or_else(|| usage.get("prompt_tokens"))
                        .and_then(|v| v.as_u64())
                        .unwrap_or(0) as u32;
                    let completion = usage
                        .get("output_tokens")
                        .or_else(|| usage.get("completion_tokens"))
                        .and_then(|v| v.as_u64())
                        .unwrap_or(0) as u32;
                    let total = usage
                        .get("total_tokens")
                        .and_then(|v| v.as_u64())
                        .unwrap_or((prompt + completion) as u64)
                        as u32;
                    ctx.usage.prompt_tokens = prompt;
                    ctx.usage.completion_tokens = completion;
                    ctx.usage.total_tokens = total;
                }
                let cost_usd = egress::compute_cost(
                    &resolved.provider_kind,
                    &resolved.model_id,
                    &ctx.usage,
                    |pk, mid| (self.deps.pricing_fn)(pk, mid),
                );
                let attempt_ms = elapsed_ms(attempt_started);
                let attempts = vec![attempt_record(
                    &resolved,
                    "ok",
                    None,
                    None,
                    Some(attempt_ms),
                    None,
                )];
                self.record_usage(
                    &ctx,
                    &resolved,
                    cost_usd,
                    false,
                    "ok",
                    None,
                    None,
                    Some("compact".into()),
                    Some(ctx.latency_ms()),
                    None,
                    &attempts,
                )
                .await;
                self.remember_session_affinity(&ctx, &resolved);
                info!(
                    request_id = %ctx.request_id,
                    alias = %ctx.request.alias,
                    provider_id = %resolved.provider_id,
                    model_id = %resolved.model_id,
                    attempt_ms,
                    "pipeline compact complete"
                );
                Ok(resp)
            }
            Err(e) => {
                self.note_upstream_error(&resolved, &e);
                let error_class = provider_error_class(&e);
                let http_status = e.http_status_hint();
                let attempt_ms = elapsed_ms(attempt_started);
                let attempts = vec![attempt_record(
                    &resolved,
                    "error",
                    Some(error_class.clone()),
                    http_status,
                    Some(attempt_ms),
                    None,
                )];
                self.record_usage(
                    &ctx,
                    &resolved,
                    0.0,
                    false,
                    "error",
                    Some(error_class),
                    http_status,
                    None,
                    Some(ctx.latency_ms()),
                    None,
                    &attempts,
                )
                .await;
                Err(GatewayError::from(e))
            }
        }
    }

    /// L2 checks with typed error metadata for HTTP status mapping.
    async fn run_ingress_checks(
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

        let quota_req = ingress::build_quota_check(&policy, &ctx.request.alias);
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

    async fn run_non_stream(
        &self,
        mut ctx: PipelineContext,
        mut resolved: ResolvedProvider,
        mut auth: UpstreamAuth,
    ) -> Result<PipelineResult, GatewayError> {
        let mut attempts: Vec<conduit_quota::QuotaAttemptRecord> = Vec::new();
        loop {
            let attempt_started = chrono::Utc::now();
            let upstream_req = request_for_upstream(&ctx.request, &resolved);
            debug!(
                request_id = %ctx.request_id,
                provider_id = %resolved.provider_id,
                provider_kind = %resolved.provider_kind,
                model_id = %resolved.model_id,
                attempt_no = resolved.attempt_no,
                stream = false,
                "pipeline upstream attempt start"
            );
            let result = dispatch_non_stream(
                &resolved,
                &upstream_req,
                &auth,
                Some(self.rate_limit_sink()),
            )
            .await;

            match result {
                Ok((resp, loss)) => {
                    let attempt_ms = elapsed_ms(attempt_started);
                    ctx.loss_report = loss;
                    ctx.merge_usage(&resp.usage);
                    let cost_usd = egress::compute_cost(
                        &resolved.provider_kind,
                        &resolved.model_id,
                        &ctx.usage,
                        |pk, mid| (self.deps.pricing_fn)(pk, mid),
                    );

                    attempts.push(attempt_record(
                        &resolved,
                        "ok",
                        None,
                        None,
                        Some(attempt_ms),
                        // Non-stream: header TTFB is not exposed by the HTTP client;
                        // leave per-try ttfb null (main row uses duration only).
                        None,
                    ));
                    info!(
                        request_id = %ctx.request_id,
                        alias = %ctx.request.alias,
                        provider_id = %resolved.provider_id,
                        model_id = %resolved.model_id,
                        attempt_no = resolved.attempt_no,
                        attempt_ms,
                        duration_ms = ctx.latency_ms(),
                        prompt_tokens = ctx.usage.prompt_tokens,
                        completion_tokens = ctx.usage.completion_tokens,
                        total_tokens = ctx.usage.total_tokens,
                        cost_usd,
                        finish_reason = %finish_reason_str(&resp.finish_reason),
                        "pipeline non-stream complete"
                    );
                    self.record_usage(
                        &ctx,
                        &resolved,
                        cost_usd,
                        false,
                        "ok",
                        None,
                        None,
                        Some(finish_reason_str(&resp.finish_reason)),
                        Some(ctx.latency_ms()),
                        None,
                        &attempts,
                    )
                    .await;
                    self.remember_session_affinity(&ctx, &resolved);
                    return Ok(PipelineResult::Complete(resp));
                }

                Err(e) => {
                    let attempt_ms = elapsed_ms(attempt_started);
                    let error_class = provider_error_class(&e);
                    let http_status = e.http_status_hint();
                    self.note_upstream_error(&resolved, &e);
                    attempts.push(attempt_record(
                        &resolved,
                        "error",
                        Some(error_class.clone()),
                        http_status,
                        Some(attempt_ms),
                        None,
                    ));
                    let table = ctx.routing_table.clone();
                    if should_retry(&mut ctx, &table, &e) {
                        debug!(
                            request_id = %ctx.request_id,
                            provider_id = %resolved.provider_id,
                            attempt_no = resolved.attempt_no,
                            next_attempt = ctx.attempt_no,
                            attempt_ms,
                            error_class = %error_class,
                            http_status = ?http_status,
                            error = %e,
                            "pipeline will retry after upstream error"
                        );
                        let preferred = self.session_preferred(&ctx);
                        let skip = self.deps.cooldown.cooling_ids();
                        if let Err(routing_err) = route_request_with_skip(
                            &mut ctx,
                            preferred.as_deref(),
                            Some(&skip),
                            Some(self.deps.pool_cursors.as_ref()),
                        ) {
                            warn!(
                                request_id = %ctx.request_id,
                                error = %routing_err,
                                "pipeline retry routing failed"
                            );
                            self.record_usage(
                                &ctx,
                                &resolved,
                                0.0,
                                false,
                                "error",
                                Some("routing".into()),
                                None,
                                None,
                                Some(ctx.latency_ms()),
                                None,
                                &attempts,
                            )
                            .await;
                            return Err(routing_err);
                        }
                        let new_resolved = ctx.resolved.as_ref().unwrap().clone();
                        debug!(
                            request_id = %ctx.request_id,
                            from_provider = %resolved.provider_id,
                            to_provider = %new_resolved.provider_id,
                            to_model = %new_resolved.model_id,
                            attempt_no = new_resolved.attempt_no,
                            "pipeline retry re-routed"
                        );
                        let client_headers = auth.client_headers.clone();
                        auth = match self.resolve_auth(&new_resolved.provider_id).await {
                            Ok(mut s) => {
                                s.client_headers = client_headers;
                                s
                            }
                            Err(ae) => {
                                debug!(
                                    request_id = %ctx.request_id,
                                    provider_id = %new_resolved.provider_id,
                                    error = %ae,
                                    "pipeline retry credential resolve failed"
                                );
                                self.record_usage(
                                    &ctx,
                                    &resolved,
                                    0.0,
                                    false,
                                    "error",
                                    Some("unauthorized".into()),
                                    Some(401),
                                    None,
                                    Some(ctx.latency_ms()),
                                    None,
                                    &attempts,
                                )
                                .await;
                                return Err(ae);
                            }
                        };
                        resolved = new_resolved;
                    } else {
                        warn!(
                            request_id = %ctx.request_id,
                            provider_id = %resolved.provider_id,
                            attempt_no = resolved.attempt_no,
                            attempt_ms,
                            error_class = %error_class,
                            http_status = ?http_status,
                            error = %e,
                            "pipeline non-stream failed (no more retries)"
                        );
                        self.record_usage(
                            &ctx,
                            &resolved,
                            0.0,
                            false,
                            "error",
                            Some(error_class),
                            http_status,
                            None,
                            Some(ctx.latency_ms()),
                            None,
                            &attempts,
                        )
                        .await;
                        return Err(GatewayError::Provider(e));
                    }
                }
            }
        }
    }

    async fn run_stream(
        &self,
        mut ctx: PipelineContext,
        mut resolved: ResolvedProvider,
        mut auth: UpstreamAuth,
    ) -> Result<PipelineResult, GatewayError> {
        let mut prior_attempts: Vec<conduit_quota::QuotaAttemptRecord> = Vec::new();
        loop {
            let attempt_started = chrono::Utc::now();
            let upstream_req = request_for_upstream(&ctx.request, &resolved);
            debug!(
                request_id = %ctx.request_id,
                provider_id = %resolved.provider_id,
                provider_kind = %resolved.provider_kind,
                model_id = %resolved.model_id,
                attempt_no = resolved.attempt_no,
                stream = true,
                "pipeline upstream attempt start"
            );
            let result = dispatch_stream(
                &resolved,
                &upstream_req,
                &auth,
                Some(self.rate_limit_sink()),
            )
            .await;

            match result {
                Ok((stream, loss)) => {
                    ctx.loss_report = loss;
                    self.remember_session_affinity(&ctx, &resolved);

                    let preferred = self.session_preferred(&ctx);
                    let route_meta =
                        route_observability(&ctx, &resolved, preferred.as_deref());
                    let attempt_count = (resolved.attempt_no + 1).max(1);
                    let open_ms = elapsed_ms(attempt_started);
                    info!(
                        request_id = %ctx.request_id,
                        alias = %ctx.request.alias,
                        provider_id = %resolved.provider_id,
                        model_id = %resolved.model_id,
                        attempt_no = resolved.attempt_no,
                        open_ms,
                        selected_reason = route_meta.selected_reason.as_deref().unwrap_or(""),
                        "pipeline stream opened"
                    );
                    let instrumented = UsageTrackingStream::new(
                        stream,
                        self.deps.pricing_fn.clone(),
                        self.deps.quota.clone(),
                        StreamRecordMeta {
                            request_id: ctx.request_id.clone(),
                            downstream_key_id: ctx.downstream_key_id.clone(),
                            alias: ctx.request.alias.clone(),
                            provider_id: resolved.provider_id.clone(),
                            provider_kind: resolved.provider_kind.clone(),
                            model_id: resolved.model_id.clone(),
                            started_at: ctx.started_at,
                            attempt_started_at: attempt_started,
                            route_strategy: route_meta.route_strategy,
                            attempt_no: resolved.attempt_no,
                            attempt_count,
                            session_id: ctx.session_id.clone(),
                            affinity_hit: route_meta.affinity_hit,
                            pool_id: route_meta.pool_id,
                            selected_reason: route_meta.selected_reason,
                            prior_attempts,
                        },
                    );
                    return Ok(PipelineResult::Streaming(Box::pin(instrumented)));
                }

                Err(e) => {
                    let attempt_ms = elapsed_ms(attempt_started);
                    let error_class = provider_error_class(&e);
                    let http_status = e.http_status_hint();
                    self.note_upstream_error(&resolved, &e);
                    prior_attempts.push(attempt_record(
                        &resolved,
                        "error",
                        Some(error_class.clone()),
                        http_status,
                        Some(attempt_ms),
                        None,
                    ));
                    let table = ctx.routing_table.clone();
                    if should_retry(&mut ctx, &table, &e) {
                        debug!(
                            request_id = %ctx.request_id,
                            provider_id = %resolved.provider_id,
                            attempt_no = resolved.attempt_no,
                            next_attempt = ctx.attempt_no,
                            attempt_ms,
                            error_class = %error_class,
                            http_status = ?http_status,
                            error = %e,
                            "pipeline stream will retry after upstream error"
                        );
                        let preferred = self.session_preferred(&ctx);
                        let skip = self.deps.cooldown.cooling_ids();
                        if let Err(routing_err) = route_request_with_skip(
                            &mut ctx,
                            preferred.as_deref(),
                            Some(&skip),
                            Some(self.deps.pool_cursors.as_ref()),
                        ) {
                            warn!(
                                request_id = %ctx.request_id,
                                error = %routing_err,
                                "pipeline stream retry routing failed"
                            );
                            self.record_usage(
                                &ctx,
                                &resolved,
                                0.0,
                                true,
                                "error",
                                Some("routing".into()),
                                None,
                                None,
                                Some(ctx.latency_ms()),
                                None,
                                &prior_attempts,
                            )
                            .await;
                            return Err(routing_err);
                        }
                        let new_resolved = ctx.resolved.as_ref().unwrap().clone();
                        debug!(
                            request_id = %ctx.request_id,
                            from_provider = %resolved.provider_id,
                            to_provider = %new_resolved.provider_id,
                            to_model = %new_resolved.model_id,
                            attempt_no = new_resolved.attempt_no,
                            "pipeline stream retry re-routed"
                        );
                        let client_headers = auth.client_headers.clone();
                        auth = match self.resolve_auth(&new_resolved.provider_id).await {
                            Ok(mut s) => {
                                s.client_headers = client_headers;
                                s
                            }
                            Err(ae) => {
                                debug!(
                                    request_id = %ctx.request_id,
                                    provider_id = %new_resolved.provider_id,
                                    error = %ae,
                                    "pipeline stream retry credential resolve failed"
                                );
                                self.record_usage(
                                    &ctx,
                                    &resolved,
                                    0.0,
                                    true,
                                    "error",
                                    Some("unauthorized".into()),
                                    Some(401),
                                    None,
                                    Some(ctx.latency_ms()),
                                    None,
                                    &prior_attempts,
                                )
                                .await;
                                return Err(ae);
                            }
                        };
                        resolved = new_resolved;
                    } else {
                        warn!(
                            request_id = %ctx.request_id,
                            provider_id = %resolved.provider_id,
                            attempt_no = resolved.attempt_no,
                            attempt_ms,
                            error_class = %error_class,
                            http_status = ?http_status,
                            error = %e,
                            "pipeline stream open failed (no more retries)"
                        );
                        self.record_usage(
                            &ctx,
                            &resolved,
                            0.0,
                            true,
                            "error",
                            Some(error_class),
                            http_status,
                            None,
                            Some(ctx.latency_ms()),
                            None,
                            &prior_attempts,
                        )
                        .await;
                        return Err(GatewayError::Provider(e));
                    }
                }
            }
        }
    }

    /// Sink for successful/error response rate-limit headers → quota snapshot store.
    fn rate_limit_sink(&self) -> conduit_upstream::RateLimitHeaderSink {
        let store = self.deps.quota_snapshots.clone();
        std::sync::Arc::new(move |provider_id: &str, headers: Vec<(String, String)>| {
            store.record_headers(provider_id, headers);
        })
    }

    /// Mark provider cooling on 429 / usage_limit so multi-target routes skip it.
    fn note_upstream_error(&self, resolved: &ResolvedProvider, err: &ProviderError) {
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

    async fn resolve_auth(&self, key_id: &str) -> Result<UpstreamAuth, GatewayError> {
        (self.deps.secret_fn)(key_id.to_string()).await
    }

    /// Session-scoped preferred provider when a session id is present.
    fn session_preferred(&self, ctx: &PipelineContext) -> Option<String> {
        let sid = ctx.session_id.as_deref()?.trim();
        if sid.is_empty() {
            return None;
        }
        self.deps.affinity.preferred(sid, &ctx.request.alias)
    }
}

/// Resolve session id from headers and request metadata (for affinity).
///
/// Public for integration tests of the live header → pin path.
pub fn resolve_session_id(
    headers: &[(String, String)],
    request: &CanonicalChatRequest,
) -> Option<String> {
    if let Some(s) = extract_session_id(headers, None) {
        return Some(s);
    }
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
    if map.is_empty() {
        return None;
    }
    extract_session_id(&[], Some(&serde_json::Value::Object(map)))
}

impl PipelineHandle {
    /// Remember successful provider for this **session** (not downstream key).
    ///
    /// Applies to multi-target fallback/weighted and any pool route. No-ops
    /// when no session id was extracted.
    fn remember_session_affinity(&self, ctx: &PipelineContext, resolved: &ResolvedProvider) {
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
    async fn record_usage(
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
            attempts: attempts.to_vec(),
        };
        if let Err(e) = self.deps.quota.record(&req).await {
            warn!(error = %e, request_id = %ctx.request_id, "usage record failed");
        }
    }
}

struct RouteObs {
    route_strategy: Option<String>,
    affinity_hit: Option<bool>,
    pool_id: Option<String>,
    selected_reason: Option<String>,
}

fn route_observability(
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

fn attempt_record(
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

fn elapsed_ms(started: chrono::DateTime<chrono::Utc>) -> u64 {
    (chrono::Utc::now() - started).num_milliseconds().max(0) as u64
}

fn request_for_upstream(
    request: &CanonicalChatRequest,
    resolved: &ResolvedProvider,
) -> CanonicalChatRequest {
    let mut req = request.clone();
    if !resolved.model_id.is_empty() {
        req.alias = resolved.model_id.clone();
    }
    req
}

// Keep WireFormat import used by callers constructing IngressWire; silence unused in this module.
#[allow(dead_code)]
fn _wire_format_touch(fmt: WireFormat) -> &'static str {
    fmt.as_str()
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod hotpath_tests {
    use std::sync::Mutex;

    use arc_swap::ArcSwap;
    use conduit_quota::engine::NoopQuotaEngine;
    use conduit_router::{
        policy::RetryPolicy,
        table::{Route, RouteTarget, RoutingStrategy},
    };
    use secrecy::SecretString;

    use super::*;

    fn sample_table(model: &str) -> RoutingTable {
        RoutingTable::new(vec![Route {
            alias: "gpt-4o".into(),
            strategy: RoutingStrategy::Fixed,
            pool_strategy: conduit_router::PoolStrategy::default(),
            targets: vec![RouteTarget {
                provider_id: "openai".into(),
                model_id: model.into(),
                provider_kind: "openai".into(),
                base_url: Some("https://api.openai.com".into()),
                weight: 1,
                request_overrides: Default::default(),
                pool_id: None,
                pool_kind: None,
            }],
            retry_policy: RetryPolicy::default(),
        }])
    }

    fn sample_request() -> CanonicalChatRequest {
        use conduit_ir::canonical::CanonicalMessage;
        CanonicalChatRequest::new("gpt-4o", vec![CanonicalMessage::user("hi")])
    }

    #[test]
    fn request_for_upstream_rewrites_alias_to_model_id() {
        let req = CanonicalChatRequest::new(
            "gpt",
            vec![conduit_ir::canonical::CanonicalMessage::user("hi")],
        );
        let resolved = ResolvedProvider {
            provider_id: "p1".into(),
            model_id: "gpt-5.1".into(),
            provider_kind: "codex-oauth".into(),
            base_url: None,
            request_overrides: Default::default(),
            attempt_no: 0,
        };
        let up = request_for_upstream(&req, &resolved);
        assert_eq!(up.alias, "gpt-5.1");
        assert_eq!(req.alias, "gpt");
    }

    fn policy_for(raw: &str, key_id: &str) -> KeyPolicyFn {
        let raw = raw.to_string();
        let key_id = key_id.to_string();
        Arc::new(move |bearer: String| {
            let raw = raw.clone();
            let key_id = key_id.clone();
            Box::pin(async move {
                if bearer == raw {
                    Ok(Some(KeyPolicy {
                        key_id,
                        model_whitelist: vec![],
                        rate_limit_rpm: None,
                    }))
                } else {
                    Ok(None)
                }
            })
        })
    }

    #[tokio::test]
    async fn missing_bearer_rejected_before_routing() {
        let handle = PipelineHandle::new(Arc::new(PipelineDeps {
            routing_table: Arc::new(ArcSwap::from_pointee(sample_table("gpt-4o"))),
            secret_fn: Arc::new(|_| {
                Box::pin(async {
                    Err(GatewayError::Internal(
                        "secret should not be called without auth".into(),
                    ))
                })
            }),
            pricing_fn: Arc::new(|_, _| None),
            quota: Arc::new(NoopQuotaEngine),
            key_policy_fn: Arc::new(|_| {
                Box::pin(async {
                    Err(GatewayError::Internal(
                        "key policy should not be called without bearer".into(),
                    ))
                })
            }),
            affinity: Arc::new(AffinityStore::new()),
            pool_cursors: Arc::new(PoolCursorStore::new()),
            cooldown: Arc::new(ProviderCooldownStore::new()),
            quota_snapshots: Arc::new(UpstreamQuotaStore::new()),
        }));

        match handle
            .run(
                sample_request(),
                None,
                vec![],
                IngressWire {
                    format: WireFormat::OpenaiChat,
                },
            )
            .await
        {
            Err(GatewayError::Unauthorized(_)) => {}
            Ok(_) => panic!("must reject missing bearer"),
            Err(e) => panic!("expected Unauthorized, got {e}"),
        }
    }

    #[tokio::test]
    async fn accepted_key_uses_stable_id_not_raw_bearer_in_quota() {
        let raw_bearer = "sk-raw-secret-MUST-NOT-APPEAR";
        let stable_id = "dk_stable_ulid_01";

        let checked_ids: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let checked_ids2 = checked_ids.clone();

        struct CapturingQuota {
            ids: Arc<Mutex<Vec<String>>>,
        }
        #[async_trait::async_trait]
        impl conduit_quota::engine::QuotaEngine for CapturingQuota {
            async fn check(
                &self,
                req: &conduit_quota::check::QuotaCheckRequest,
            ) -> Result<(), conduit_ir::error::QuotaError> {
                self.ids.lock().unwrap().push(req.downstream_key_id.clone());
                Ok(())
            }
            async fn record(
                &self,
                _req: &conduit_quota::check::QuotaRecordRequest,
            ) -> Result<(), conduit_ir::error::QuotaError> {
                Ok(())
            }
        }

        let handle = PipelineHandle::new(Arc::new(PipelineDeps {
            routing_table: Arc::new(ArcSwap::from_pointee(sample_table("gpt-4o"))),
            secret_fn: Arc::new(move |_| {
                Box::pin(async {
                    Ok(UpstreamAuth {
                        token: SecretString::new("upstream-token".into()),
                        extra_headers: vec![],
                        client_headers: vec![],
                        using_api: false,
                    })
                })
            }),
            pricing_fn: Arc::new(|_, _| None),
            quota: Arc::new(CapturingQuota { ids: checked_ids2 }),
            key_policy_fn: policy_for(raw_bearer, stable_id),
            affinity: Arc::new(AffinityStore::new()),
            pool_cursors: Arc::new(PoolCursorStore::new()),
            cooldown: Arc::new(ProviderCooldownStore::new()),
            quota_snapshots: Arc::new(UpstreamQuotaStore::new()),
        }));

        let _ = handle
            .run(
                sample_request(),
                Some(raw_bearer.into()),
                vec![],
                IngressWire {
                    format: WireFormat::OpenaiChat,
                },
            )
            .await;

        let ids = checked_ids.lock().unwrap().clone();
        assert_eq!(ids, vec![stable_id.to_string()]);
        assert!(!ids.iter().any(|id| id == raw_bearer));
    }

    #[test]
    fn compute_cost_driven_from_pipeline_egress() {
        let usage = conduit_ir::canonical::Usage {
            prompt_tokens: 1_000_000,
            completion_tokens: 0,
            total_tokens: 1_000_000,
            reasoning_tokens: 0,
            cache_read_tokens: 0,
            cache_write_tokens: 0,
        };
        let cost = egress::compute_cost("openai", "gpt-4o", &usage, |_, _| {
            Some(egress::ModelPricing {
                input_per_mtok: 3.0,
                output_per_mtok: 6.0,
                cache_read_per_mtok: None,
                cache_write_per_mtok: None,
                reasoning_per_mtok: None,
            })
        });
        assert!((cost - 3.0).abs() < 1e-12);
    }
}
