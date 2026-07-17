//! PipelineHandle: main entry point for running requests through the L2-L7 pipeline.

use std::{future::Future, pin::Pin, sync::Arc};

use conduit_codec::{anthropic::AnthropicCodec, openai::OpenAiCodec, WireCodec};
use conduit_ir::{
    canonical::{CanonicalChatRequest, CanonicalChatResponse, CanonicalChunk},
    error::{GatewayError, ProviderError},
    loss::LossReport,
    trace::{TraceEvent, TraceEventKind, WireFormat},
};
use conduit_router::{table::RoutingTable, AffinityStore};
use conduit_trace::sink::TraceSink;
use futures::stream::BoxStream;
use tracing::warn;

use super::{
    context::{client_response_headers, IngressWire, PipelineContext, ResolvedProvider},
    egress,
    ingress::{self, KeyPolicy},
    provider::{dispatch_non_stream, dispatch_stream, UpstreamAuth},
    stage::{route_request, should_retry},
    stream_probe::InstrumentedStream,
};

// ── Async dependency types (no nested block_on on the hot path) ───────────────

pub type BoxFut<T> = Pin<Box<dyn Future<Output = T> + Send + 'static>>;

/// Resolve raw bearer → key policy (or None if unknown). DB/async only.
pub type KeyPolicyFn =
    Arc<dyn Fn(String) -> BoxFut<Result<Option<KeyPolicy>, GatewayError>> + Send + Sync>;

/// Resolve upstream_key_id → token + optional OAuth headers.
pub type AuthFn = Arc<dyn Fn(String) -> BoxFut<Result<UpstreamAuth, GatewayError>> + Send + Sync>;

/// Pure in-memory pricing lookup (sync). Must not nest block_on / DB I/O.
pub use super::stream_probe::PricingFn;

/// Dependencies injected into the pipeline at startup.
///
/// `routing_table` is an [`arc_swap::ArcSwap`] so admin reloads can publish a
/// new snapshot without per-request deep clones or write locks on the hot path.
pub struct PipelineDeps {
    pub routing_table: Arc<arc_swap::ArcSwap<RoutingTable>>,
    pub trace_sink: Arc<TraceSink>,
    /// upstream_key_id → UpstreamAuth (API key or OAuth access token + headers)
    pub secret_fn: AuthFn,
    /// pricing lookup for egress cost calculation (sync memory table)
    pub pricing_fn: PricingFn,
    /// quota engine (check before + record after)
    pub quota: Arc<dyn conduit_quota::engine::QuotaEngine>,
    /// raw bearer → key policy lookup
    pub key_policy_fn: KeyPolicyFn,
    /// Sticky provider pins for multi-target fallback / weighted routes (process-local).
    pub affinity: Arc<AffinityStore>,
}

/// The final result of running a request through the pipeline.
pub enum PipelineResult {
    /// Non-streaming response. Trace events have already been flushed to the sink.
    Complete(CanonicalChatResponse),
    /// Streaming response. The stream emits trace events and records quota usage
    /// once fully consumed.
    Streaming(BoxStream<'static, Result<CanonicalChunk, ProviderError>>),
}

pub struct PipelineHandle {
    deps: Arc<PipelineDeps>,
}

impl PipelineHandle {
    pub fn new(deps: Arc<PipelineDeps>) -> Self {
        Self { deps }
    }

    /// Shared deps (for tests / introspection).
    pub fn deps(&self) -> &PipelineDeps {
        &self.deps
    }

    /// Run a chat request through the full pipeline.
    ///
    /// `downstream_bearer` is the raw Authorization bearer secret (if any).
    /// After successful auth it is **never** stored on the context; only the
    /// stable DB key id is used for quota, traces, and FinalUsage.
    ///
    /// `client_headers` are selected downstream HTTP headers (User-Agent,
    /// Stainless, Anthropic-Beta, …) for Claude OAuth device-profile parity.
    ///
    /// `ingress_wire` is the original client body + protocol so traces preserve
    /// the real request/response wire format (not only the IR).
    pub async fn run(
        &self,
        request: conduit_ir::canonical::CanonicalChatRequest,
        downstream_bearer: Option<String>,
        client_headers: Vec<(String, String)>,
        ingress_wire: IngressWire,
    ) -> Result<PipelineResult, GatewayError> {
        // L2: mandatory auth → stable key identity → quota pre-check
        let raw = ingress::require_bearer(downstream_bearer.as_deref())?;
        let policy_opt = (self.deps.key_policy_fn)(raw.to_string()).await?;
        let policy = ingress::accept_policy(raw, policy_opt)?;
        policy.check_model_allowed(&request.alias)?;

        let quota_req = ingress::build_quota_check(&policy, &request.alias);
        self.deps
            .quota
            .check(&quota_req)
            .await
            .map_err(GatewayError::from)?;

        // Stable ledger / audit identity only (never the raw bearer).
        let ledger_key_id = policy.key_id.clone();

        // Lock-free load of the current routing snapshot (admin stores a new Arc).
        let table_snap = self.deps.routing_table.load_full();
        let stream = request.stream;
        let alias = request.alias.clone();

        let mut ctx = PipelineContext::new(request, Some(ledger_key_id), table_snap)
            .with_ingress_wire(ingress_wire.clone());

        // Complete audit: original wire body + IR on RequestReceived.
        let request_ir =
            serde_json::to_value(&ctx.request).unwrap_or_else(|_| serde_json::json!({}));
        if let Err(e) = self.deps.trace_sink.send(TraceEvent::with_trace_id(
            ctx.trace_id.clone(),
            TraceEventKind::RequestReceived {
                downstream_key_id: ctx.downstream_key_id.clone(),
                alias,
                stream,
                request: ingress_wire.body.clone(),
                request_ir: Some(request_ir),
                wire_format: Some(ingress_wire.format.to_string()),
                request_headers: Some(ingress_wire.headers.clone()),
            },
        )) {
            warn!(error = %e, "failed to enqueue RequestReceived audit event");
        }

        // L3: initial routing decision (affinity pin from last success for this key+alias)
        let preferred = ctx
            .downstream_key_id
            .as_deref()
            .and_then(|k| self.deps.affinity.preferred(k, &ctx.request.alias));
        if let Err(e) = route_request(&mut ctx, preferred.as_deref()) {
            if let Err(se) = self.deps.trace_sink.send(TraceEvent::with_trace_id(
                ctx.trace_id.clone(),
                TraceEventKind::Error {
                    kind: "RoutingError".into(),
                    message: e.to_string(),
                },
            )) {
                warn!(error = %se, "trace sink enqueue failed; RoutingError event dropped");
            }
            return Err(e);
        }

        let resolved = ctx.resolved.as_ref().unwrap().clone();
        let mut auth = match self.resolve_auth(&resolved.upstream_key_id).await {
            Ok(a) => a,
            Err(e) => {
                if let Err(se) = self.deps.trace_sink.send(TraceEvent::with_trace_id(
                    ctx.trace_id.clone(),
                    TraceEventKind::Error {
                        kind: "AuthError".into(),
                        message: e.to_string(),
                    },
                )) {
                    warn!(error = %se, "trace sink enqueue failed; AuthError event dropped");
                }
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

    // ── Non-streaming path ──────────────────────────────────────────────────

    async fn run_non_stream(
        &self,
        mut ctx: PipelineContext,
        mut resolved: ResolvedProvider,
        mut auth: UpstreamAuth,
    ) -> Result<PipelineResult, GatewayError> {
        loop {
            // Wire codecs put `req.alias` into the upstream `model` field. Clients
            // send the **route alias** (e.g. "gpt"); targets carry the real
            // upstream model_id (e.g. "gpt-5.1"). Rewrite before dispatch so
            // Codex/OpenAI/Anthropic see the routed model, not the gateway alias.
            let upstream_req = request_for_upstream(&ctx.request, &resolved);
            let result = dispatch_non_stream(&resolved, &upstream_req, &auth).await;

            match result {
                Ok((resp, loss)) => {
                    attach_attempt_loss(&mut ctx.events, &loss);
                    ctx.loss_report = loss;
                    ctx.merge_usage(&resp.usage);
                    let cost_usd = egress::compute_cost(
                        &resolved.provider_kind,
                        &resolved.model_id,
                        &ctx.usage,
                        |pk, mid| (self.deps.pricing_fn)(pk, mid),
                    );
                    let wire_fmt = ctx
                        .ingress_wire
                        .as_ref()
                        .map(|w| w.format)
                        .unwrap_or(WireFormat::OpenaiChat);
                    let response_json = audit_encode_response(wire_fmt, &resp);
                    ctx.push_event(TraceEventKind::UpstreamResponse {
                        status: 200,
                        latency_ms: ctx.latency_ms(),
                        ttfb_ms: None,
                        response: Some(response_json),
                        wire_format: Some(wire_fmt.to_string()),
                        stream: false,
                        stream_frames: None,
                        response_headers: Some(client_response_headers(wire_fmt, false)),
                    });
                    egress::finalize(&mut ctx, cost_usd);

                    // Usage ledger is independent of traces (which may be toggled off later).
                    self.record_usage(&ctx, &resolved, cost_usd, /* stream */ false)
                        .await;
                    self.remember_affinity(&ctx, &resolved);

                    self.flush_events(&mut ctx);

                    return Ok(PipelineResult::Complete(resp));
                }

                Err(e) => {
                    let table = ctx.routing_table.clone();
                    if should_retry(&mut ctx, &table, &e) {
                        let preferred = ctx
                            .downstream_key_id
                            .as_deref()
                            .and_then(|k| self.deps.affinity.preferred(k, &ctx.request.alias));
                        if let Err(routing_err) = route_request(&mut ctx, preferred.as_deref()) {
                            self.flush_events_with_error(&mut ctx, routing_err.to_string());
                            return Err(routing_err);
                        }
                        let new_resolved = ctx.resolved.as_ref().unwrap().clone();
                        let client_headers = auth.client_headers.clone();
                        auth = match self.resolve_auth(&new_resolved.upstream_key_id).await {
                            Ok(mut s) => {
                                s.client_headers = client_headers;
                                s
                            }
                            Err(e) => {
                                self.flush_events_with_error(&mut ctx, e.to_string());
                                return Err(e);
                            }
                        };
                        resolved = new_resolved;
                    } else {
                        self.flush_events_with_error(&mut ctx, e.to_string());
                        return Err(GatewayError::Provider(e));
                    }
                }
            }
        }
    }

    // ── Streaming path ──────────────────────────────────────────────────────

    async fn run_stream(
        &self,
        mut ctx: PipelineContext,
        mut resolved: ResolvedProvider,
        mut auth: UpstreamAuth,
    ) -> Result<PipelineResult, GatewayError> {
        loop {
            let upstream_req = request_for_upstream(&ctx.request, &resolved);
            let result = dispatch_stream(&resolved, &upstream_req, &auth).await;

            match result {
                Ok((stream, loss)) => {
                    attach_attempt_loss(&mut ctx.events, &loss);
                    ctx.loss_report = loss;
                    // Pin the provider that accepted the stream; stream may still
                    // fail mid-body, but opening is the same sticky signal as chat.
                    self.remember_affinity(&ctx, &resolved);
                    self.flush_events(&mut ctx);

                    let wire_fmt = ctx
                        .ingress_wire
                        .as_ref()
                        .map(|w| w.format)
                        .unwrap_or(WireFormat::OpenaiChat);
                    let instrumented = InstrumentedStream::new(
                        stream,
                        self.deps.trace_sink.clone(),
                        self.deps.pricing_fn.clone(),
                        self.deps.quota.clone(),
                        ctx.downstream_key_id.clone(),
                        ctx.started_at,
                        ctx.request.alias.clone(),
                        resolved.provider_id.clone(),
                        resolved.provider_kind.clone(),
                        resolved.model_id.clone(),
                        ctx.loss_report.clone(),
                        ctx.trace_id.clone(),
                        wire_fmt,
                    );

                    return Ok(PipelineResult::Streaming(Box::pin(instrumented)));
                }

                Err(e) => {
                    let table = ctx.routing_table.clone();
                    if should_retry(&mut ctx, &table, &e) {
                        let preferred = ctx
                            .downstream_key_id
                            .as_deref()
                            .and_then(|k| self.deps.affinity.preferred(k, &ctx.request.alias));
                        if let Err(routing_err) = route_request(&mut ctx, preferred.as_deref()) {
                            self.flush_events_with_error(&mut ctx, routing_err.to_string());
                            return Err(routing_err);
                        }
                        let new_resolved = ctx.resolved.as_ref().unwrap().clone();
                        let client_headers = auth.client_headers.clone();
                        auth = match self.resolve_auth(&new_resolved.upstream_key_id).await {
                            Ok(mut s) => {
                                s.client_headers = client_headers;
                                s
                            }
                            Err(e) => {
                                self.flush_events_with_error(&mut ctx, e.to_string());
                                return Err(e);
                            }
                        };
                        resolved = new_resolved;
                    } else {
                        self.flush_events_with_error(&mut ctx, e.to_string());
                        return Err(GatewayError::Provider(e));
                    }
                }
            }
        }
    }

    // ── Helpers ─────────────────────────────────────────────────────────────

    async fn resolve_auth(&self, key_id: &str) -> Result<UpstreamAuth, GatewayError> {
        (self.deps.secret_fn)(key_id.to_string()).await
    }

    /// Sticky pin for multi-target fallback / weighted (fixed ignores pins).
    fn remember_affinity(&self, ctx: &PipelineContext, resolved: &ResolvedProvider) {
        let alias = ctx.request.alias.as_str();
        let uses_sticky = ctx
            .routing_table
            .get(alias)
            .map(|r| {
                matches!(
                    r.strategy,
                    conduit_router::table::RoutingStrategy::Fallback
                        | conduit_router::table::RoutingStrategy::Weighted
                ) && r.targets.len() > 1
            })
            .unwrap_or(false);
        if !uses_sticky {
            return;
        }
        let key = ctx
            .downstream_key_id
            .as_deref()
            .filter(|s| !s.is_empty())
            .unwrap_or("_anonymous");
        self.deps
            .affinity
            .remember(key, alias, &resolved.provider_id);
    }

    /// Persist request consumption to the usage ledger (not the trace log).
    async fn record_usage(
        &self,
        ctx: &PipelineContext,
        resolved: &ResolvedProvider,
        cost_usd: f64,
        stream: bool,
    ) {
        let key_id = ctx
            .downstream_key_id
            .clone()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "_anonymous".into());
        let req = conduit_quota::QuotaRecordRequest {
            request_id: ctx.trace_id.clone(),
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
        };
        if let Err(e) = self.deps.quota.record(&req).await {
            warn!(error = %e, request_id = %ctx.trace_id, "usage record failed");
        }
    }

    fn flush_events(&self, ctx: &mut PipelineContext) {
        for kind in ctx.events.drain(..) {
            if let Err(e) = self
                .deps
                .trace_sink
                .send(TraceEvent::with_trace_id(ctx.trace_id.clone(), kind))
            {
                warn!(error = %e, "trace sink enqueue failed; event dropped");
            }
        }
    }

    fn flush_events_with_error(&self, ctx: &mut PipelineContext, message: String) {
        ctx.push_event(TraceEventKind::Error {
            kind: "GatewayError".into(),
            message,
        });
        self.flush_events(ctx);
    }
}

/// Attach a codec `LossReport` to the most recent `RoutingDecided` event.
fn attach_attempt_loss(events: &mut [TraceEventKind], loss: &LossReport) {
    for ev in events.iter_mut().rev() {
        if let TraceEventKind::RoutingDecided { attempt_loss, .. } = ev {
            *attempt_loss = Some(loss.clone());
            break;
        }
    }
}

/// Clone the client request for upstream, substituting the routed `model_id`
/// into `alias` (the field wire codecs serialize as `model`).
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

/// Encode the client-facing wire response matching the ingress protocol.
fn audit_encode_response(fmt: WireFormat, resp: &CanonicalChatResponse) -> serde_json::Value {
    match fmt {
        WireFormat::OpenaiChat => OpenAiCodec::encode_response(resp),
        WireFormat::AnthropicMessages => AnthropicCodec::encode_response(resp),
        _ => OpenAiCodec::encode_response(resp),
    }
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
    use conduit_trace::{sink::TraceSink, TraceStore};
    use secrecy::SecretString;

    use super::*;

    fn sample_table(model: &str) -> RoutingTable {
        RoutingTable::new(vec![Route {
            alias: "gpt-4o".into(),
            strategy: RoutingStrategy::Fixed,
            targets: vec![RouteTarget {
                provider_id: "openai".into(),
                model_id: model.into(),
                upstream_key_id: "k1".into(),
                provider_kind: "openai".into(),
                base_url: Some("https://api.openai.com".into()),
                weight: 1,
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
            upstream_key_id: "k1".into(),
            provider_kind: "codex-oauth".into(),
            base_url: None,
            attempt_no: 0,
        };
        let up = request_for_upstream(&req, &resolved);
        assert_eq!(up.alias, "gpt-5.1");
        // Client-facing alias on the original request is unchanged.
        assert_eq!(req.alias, "gpt");
    }

    async fn make_sink() -> Arc<TraceSink> {
        let tmp = tempfile::tempdir().unwrap();
        let store = Arc::new(TraceStore::open(tmp.path().to_path_buf()).await.unwrap());
        let (sink, _h) = TraceSink::start(store).await;
        // Keep tempdir alive by leaking (tests are short-lived).
        std::mem::forget(tmp);
        Arc::new(sink)
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

    #[test]
    fn pipeline_deps_load_routing_without_per_request_clone() {
        let swap = Arc::new(ArcSwap::from_pointee(sample_table("gpt-4o")));
        let snap = swap.load_full();
        assert_eq!(snap.get("gpt-4o").unwrap().targets[0].model_id, "gpt-4o");
        swap.store(Arc::new(sample_table("gpt-4o-mini")));
        let snap2 = swap.load_full();
        assert_eq!(
            snap2.get("gpt-4o").unwrap().targets[0].model_id,
            "gpt-4o-mini"
        );
        assert_eq!(snap.get("gpt-4o").unwrap().targets[0].model_id, "gpt-4o");
    }

    #[tokio::test]
    async fn missing_bearer_rejected_before_routing() {
        let sink = make_sink().await;
        let handle = PipelineHandle::new(Arc::new(PipelineDeps {
            routing_table: Arc::new(ArcSwap::from_pointee(sample_table("gpt-4o"))),
            trace_sink: sink,
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
        }));

        match handle
            .run(
                sample_request(),
                None,
                vec![],
                IngressWire {
                    format: WireFormat::OpenaiChat,
                    body: serde_json::json!({"model":"gpt-4o","messages":[]}),
                    headers: serde_json::json!({}),
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
    async fn invalid_bearer_rejected_before_routing() {
        let sink = make_sink().await;
        let handle = PipelineHandle::new(Arc::new(PipelineDeps {
            routing_table: Arc::new(ArcSwap::from_pointee(sample_table("gpt-4o"))),
            trace_sink: sink,
            secret_fn: Arc::new(|_| {
                Box::pin(async { Err(GatewayError::Internal("no secret".into())) })
            }),
            pricing_fn: Arc::new(|_, _| None),
            quota: Arc::new(NoopQuotaEngine),
            key_policy_fn: Arc::new(|_| Box::pin(async { Ok(None) })),
            affinity: Arc::new(AffinityStore::new()),
        }));

        match handle
            .run(
                sample_request(),
                Some("garbage-key".into()),
                vec![],
                IngressWire {
                    format: WireFormat::OpenaiChat,
                    body: serde_json::json!({"model":"gpt-4o"}),
                    headers: serde_json::json!({}),
                },
            )
            .await
        {
            Err(GatewayError::Unauthorized(_)) => {}
            Ok(_) => panic!("must reject invalid bearer"),
            Err(e) => panic!("expected Unauthorized, got {e}"),
        }
    }

    /// Accepted key → quota check scope id equals FinalUsage identity and is
    /// not the raw bearer. Exercises shipped `run` ingress path up to secret
    /// resolve (upstream call not required: secret fails after identity is set).
    #[tokio::test]
    async fn accepted_key_uses_stable_id_not_raw_bearer_in_context_and_quota() {
        let raw_bearer = "sk-raw-secret-MUST-NOT-APPEAR";
        let stable_id = "dk_stable_ulid_01";
        let sink = make_sink().await;

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
            trace_sink: sink,
            secret_fn: Arc::new(move |_| {
                // Capture that resolve was reached (auth passed).
                Box::pin(async {
                    Ok(UpstreamAuth {
                        token: SecretString::new("upstream-token".into()),
                        extra_headers: vec![],
                        client_headers: vec![],
                    })
                })
            }),
            pricing_fn: Arc::new(|_, _| None),
            quota: Arc::new(CapturingQuota { ids: checked_ids2 }),
            key_policy_fn: policy_for(raw_bearer, stable_id),
            affinity: Arc::new(AffinityStore::new()),
        }));

        // Will fail at upstream (no mock server) but identity path must run first.
        let result = handle
            .run(
                sample_request(),
                Some(raw_bearer.into()),
                vec![],
                IngressWire {
                    format: WireFormat::OpenaiChat,
                    body: serde_json::json!({"model":"gpt-4o","messages":[]}),
                    headers: serde_json::json!({}),
                },
            )
            .await;

        let ids = checked_ids.lock().unwrap().clone();
        assert_eq!(ids, vec![stable_id.to_string()]);
        assert!(!ids.iter().any(|id| id == raw_bearer));

        // Regardless of upstream outcome, if we got an error event trail the
        // RequestReceived should already have been enqueued with stable id.
        // When upstream fails we still verify check used stable id (above).
        let _ = result; // may be Err(Provider) — identity already proven
    }

    #[tokio::test]
    async fn finalize_event_carries_stable_key_id_only() {
        use conduit_ir::{canonical::Usage, loss::LossReport};

        let raw = "sk-never-in-final-usage";
        let stable = "key_stable_final";
        let mut ctx = PipelineContext::new(
            sample_request(),
            Some(stable.into()),
            Arc::new(sample_table("gpt-4o")),
        );
        // Simulate what handle does after accept_policy.
        assert_eq!(ctx.downstream_key_id.as_deref(), Some(stable));
        assert_ne!(ctx.downstream_key_id.as_deref(), Some(raw));

        ctx.usage = Usage {
            prompt_tokens: 10,
            completion_tokens: 5,
            ..Default::default()
        };
        ctx.loss_report = LossReport::default();
        egress::finalize(&mut ctx, 0.01);

        match ctx.events.last() {
            Some(TraceEventKind::FinalUsage {
                downstream_key_id: Some(id),
                ..
            }) => {
                assert_eq!(id, stable);
                assert_ne!(id, raw);
            }
            other => panic!("expected FinalUsage with stable id, got {other:?}"),
        }
    }
}
