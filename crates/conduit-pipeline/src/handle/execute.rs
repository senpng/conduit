//! Upstream execute loops (non-stream / stream) with shared failover.

use conduit_ir::error::{GatewayError, ProviderError};
use tracing::{debug, info, warn};

use super::super::{
    context::{PipelineContext, ResolvedProvider},
    egress,
    provider::{dispatch_non_stream, dispatch_stream, UpstreamAuth},
    stage::{route_request_with_skip, should_retry},
    stream_probe::{
        finish_reason_str, provider_error_class, StreamRecordMeta, UsageTrackingStream,
    },
};
use super::support::{
    attempt_record, elapsed_ms, log_codec_losses, request_for_upstream, route_observability,
};
use super::{PipelineHandle, PipelineResult};

impl PipelineHandle {
    pub(crate) async fn run_non_stream(
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
                    ctx.loss_report.merge(loss);
                    log_codec_losses(&ctx);
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
                    if let Err(fail) = self
                        .failover_after_error(
                            &mut ctx,
                            &mut resolved,
                            &mut auth,
                            e,
                            false,
                            &attempts,
                            attempt_ms,
                            &error_class,
                            http_status,
                        )
                        .await
                    {
                        return Err(fail);
                    }
                }
            }
        }
    }

    pub(crate) async fn run_stream(
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
                    ctx.loss_report.merge(loss);
                    log_codec_losses(&ctx);
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
                            loss_count: ctx.loss_report.len() as u32,
                            wire_format: ctx
                                .ingress_wire
                                .as_ref()
                                .map(|w| w.format.as_str().to_string()),
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
                    if let Err(fail) = self
                        .failover_after_error(
                            &mut ctx,
                            &mut resolved,
                            &mut auth,
                            e,
                            true,
                            &prior_attempts,
                            attempt_ms,
                            &error_class,
                            http_status,
                        )
                        .await
                    {
                        return Err(fail);
                    }
                }
            }
        }
    }

    /// Shared retry path after an upstream error.
    ///
    /// On `Ok(())`, `resolved`/`auth` are updated for the next attempt.
    /// On `Err`, usage has already been recorded and the caller must return.
    async fn failover_after_error(
        &self,
        ctx: &mut PipelineContext,
        resolved: &mut ResolvedProvider,
        auth: &mut UpstreamAuth,
        err: ProviderError,
        stream: bool,
        attempts: &[conduit_quota::QuotaAttemptRecord],
        attempt_ms: u64,
        error_class: &str,
        http_status: Option<u16>,
    ) -> Result<(), GatewayError> {
        let table = ctx.routing_table.clone();
        if !should_retry(ctx, &table, &err) {
            warn!(
                request_id = %ctx.request_id,
                provider_id = %resolved.provider_id,
                attempt_no = resolved.attempt_no,
                attempt_ms,
                error_class = %error_class,
                http_status = ?http_status,
                error = %err,
                stream,
                "pipeline failed (no more retries)"
            );
            self.record_usage(
                ctx,
                resolved,
                0.0,
                stream,
                "error",
                Some(error_class.to_string()),
                http_status,
                None,
                Some(ctx.latency_ms()),
                None,
                attempts,
            )
            .await;
            return Err(GatewayError::Provider(err));
        }

        debug!(
            request_id = %ctx.request_id,
            provider_id = %resolved.provider_id,
            attempt_no = resolved.attempt_no,
            next_attempt = ctx.attempt_no,
            attempt_ms,
            error_class = %error_class,
            http_status = ?http_status,
            error = %err,
            stream,
            "pipeline will retry after upstream error"
        );
        let preferred = self.session_preferred(ctx);
        let skip = self.deps.cooldown.cooling_ids();
        if let Err(routing_err) = route_request_with_skip(
            ctx,
            preferred.as_deref(),
            Some(&skip),
            Some(self.deps.pool_cursors.as_ref()),
        ) {
            warn!(
                request_id = %ctx.request_id,
                error = %routing_err,
                stream,
                "pipeline retry routing failed"
            );
            self.record_usage(
                ctx,
                resolved,
                0.0,
                stream,
                "error",
                Some("routing".into()),
                None,
                None,
                Some(ctx.latency_ms()),
                None,
                attempts,
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
            stream,
            "pipeline retry re-routed"
        );
        let client_headers = auth.client_headers.clone();
        *auth = match self.resolve_auth(&new_resolved.provider_id).await {
            Ok(mut s) => {
                s.client_headers = client_headers;
                s
            }
            Err(ae) => {
                debug!(
                    request_id = %ctx.request_id,
                    provider_id = %new_resolved.provider_id,
                    error = %ae,
                    stream,
                    "pipeline retry credential resolve failed"
                );
                self.record_usage(
                    ctx,
                    resolved,
                    0.0,
                    stream,
                    "error",
                    Some("unauthorized".into()),
                    Some(401),
                    None,
                    Some(ctx.latency_ms()),
                    None,
                    attempts,
                )
                .await;
                return Err(ae);
            }
        };
        *resolved = new_resolved;
        Ok(())
    }
}
