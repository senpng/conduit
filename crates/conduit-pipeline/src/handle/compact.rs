//! Responses compact path (`POST /v1/responses/compact`).

use conduit_ir::{
    error::GatewayError,
    wire_format::WireFormat,
};
use tracing::{debug, info};

use super::super::{
    context::{IngressWire, PipelineContext},
    egress,
    provider::dispatch_responses_compact,
    stage::route_request_with_skip,
    stream_probe::provider_error_class,
};
use super::support::{attempt_record, elapsed_ms, resolve_session_id};
// resolve_session_id used below
use super::PipelineHandle;

impl PipelineHandle {
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
        request_id: String,
    ) -> Result<serde_json::Value, GatewayError> {
        use conduit_ir::canonical::CanonicalChatRequest;

        let table_snap = self.deps.routing_table.load_full();
        // Minimal IR request for routing / quota / session only.
        let mut request = CanonicalChatRequest::new(alias, vec![]);
        // Prefer the ingress-minted id so compact logs share the same rid as the
        // gateway response header / route span.
        if !request_id.is_empty() {
            request.id = request_id;
        }
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
        tracing::Span::current().record("request_id", tracing::field::display(&ctx.request_id));

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

}
