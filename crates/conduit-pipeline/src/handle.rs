//! PipelineHandle: main entry point for running requests through the L2-L7 pipeline.

use std::{future::Future, pin::Pin, sync::Arc};

use conduit_ir::{
    canonical::{CanonicalChatRequest, CanonicalChatResponse, CanonicalChunk},
    error::{GatewayError, ProviderError},
    wire_format::WireFormat,
};
use conduit_router::{
    is_usage_limit_body, parse_cooldown_duration, table::RoutingTable, AffinityStore,
    ProviderCooldownStore, UpstreamQuotaStore,
};
use futures::stream::BoxStream;
use tracing::warn;

use super::{
    context::{IngressWire, PipelineContext, ResolvedProvider},
    egress,
    ingress::{self, KeyPolicy},
    provider::{dispatch_non_stream, dispatch_stream, UpstreamAuth},
    stage::{route_request_with_skip, should_retry},
    stream_probe::UsageTrackingStream,
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
    pub affinity: Arc<AffinityStore>,
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
        let mut ctx =
            PipelineContext::new(request, None, table_snap, wire_format).with_ingress_wire(ingress_wire);

        if let Err((_error_type, _http_status, err)) =
            self.run_ingress_checks(&mut ctx, downstream_bearer).await
        {
            return Err(err);
        }

        let preferred = ctx
            .downstream_key_id
            .as_deref()
            .and_then(|k| self.deps.affinity.preferred(k, &ctx.request.alias));
        let skip = self.deps.cooldown.cooling_ids();
        if let Err(e) = route_request_with_skip(&mut ctx, preferred.as_deref(), Some(&skip)) {
            return Err(e);
        }

        let resolved = ctx.resolved.as_ref().unwrap().clone();
        let mut auth = match self.resolve_auth(&resolved.upstream_key_id).await {
            Ok(a) => a,
            Err(e) => return Err(e),
        };
        auth.client_headers = client_headers;

        if stream {
            self.run_stream(ctx, resolved, auth).await
        } else {
            self.run_non_stream(ctx, resolved, auth).await
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

        ctx.downstream_key_id = Some(policy.key_id.clone());
        Ok(())
    }

    async fn run_non_stream(
        &self,
        mut ctx: PipelineContext,
        mut resolved: ResolvedProvider,
        mut auth: UpstreamAuth,
    ) -> Result<PipelineResult, GatewayError> {
        loop {
            let upstream_req = request_for_upstream(&ctx.request, &resolved);
            let result = dispatch_non_stream(
                &resolved,
                &upstream_req,
                &auth,
                Some(self.rate_limit_sink()),
            )
            .await;

            match result {
                Ok((resp, loss)) => {
                    ctx.loss_report = loss;
                    ctx.merge_usage(&resp.usage);
                    let cost_usd = egress::compute_cost(
                        &resolved.provider_kind,
                        &resolved.model_id,
                        &ctx.usage,
                        |pk, mid| (self.deps.pricing_fn)(pk, mid),
                    );

                    self.record_usage(&ctx, &resolved, cost_usd, false).await;
                    self.remember_affinity(&ctx, &resolved);
                    return Ok(PipelineResult::Complete(resp));
                }

                Err(e) => {
                    self.note_upstream_error(&resolved, &e);
                    let table = ctx.routing_table.clone();
                    if should_retry(&mut ctx, &table, &e) {
                        let preferred = ctx
                            .downstream_key_id
                            .as_deref()
                            .and_then(|k| self.deps.affinity.preferred(k, &ctx.request.alias));
                        let skip = self.deps.cooldown.cooling_ids();
                        if let Err(routing_err) =
                            route_request_with_skip(&mut ctx, preferred.as_deref(), Some(&skip))
                        {
                            return Err(routing_err);
                        }
                        let new_resolved = ctx.resolved.as_ref().unwrap().clone();
                        let client_headers = auth.client_headers.clone();
                        auth = match self.resolve_auth(&new_resolved.upstream_key_id).await {
                            Ok(mut s) => {
                                s.client_headers = client_headers;
                                s
                            }
                            Err(ae) => return Err(ae),
                        };
                        resolved = new_resolved;
                    } else {
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
        loop {
            let upstream_req = request_for_upstream(&ctx.request, &resolved);
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
                    self.remember_affinity(&ctx, &resolved);

                    let instrumented = UsageTrackingStream::new(
                        stream,
                        self.deps.pricing_fn.clone(),
                        self.deps.quota.clone(),
                        ctx.request_id.clone(),
                        ctx.downstream_key_id.clone(),
                        ctx.started_at,
                        ctx.request.alias.clone(),
                        resolved.provider_id.clone(),
                        resolved.provider_kind.clone(),
                        resolved.model_id.clone(),
                    );
                    return Ok(PipelineResult::Streaming(Box::pin(instrumented)));
                }

                Err(e) => {
                    self.note_upstream_error(&resolved, &e);
                    let table = ctx.routing_table.clone();
                    if should_retry(&mut ctx, &table, &e) {
                        let preferred = ctx
                            .downstream_key_id
                            .as_deref()
                            .and_then(|k| self.deps.affinity.preferred(k, &ctx.request.alias));
                        let skip = self.deps.cooldown.cooling_ids();
                        if let Err(routing_err) =
                            route_request_with_skip(&mut ctx, preferred.as_deref(), Some(&skip))
                        {
                            return Err(routing_err);
                        }
                        let new_resolved = ctx.resolved.as_ref().unwrap().clone();
                        let client_headers = auth.client_headers.clone();
                        auth = match self.resolve_auth(&new_resolved.upstream_key_id).await {
                            Ok(mut s) => {
                                s.client_headers = client_headers;
                                s
                            }
                            Err(ae) => return Err(ae),
                        };
                        resolved = new_resolved;
                    } else {
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
        };
        if let Err(e) = self.deps.quota.record(&req).await {
            warn!(error = %e, request_id = %ctx.request_id, "usage record failed");
        }
    }
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
            targets: vec![RouteTarget {
                provider_id: "openai".into(),
                model_id: model.into(),
                upstream_key_id: "k1".into(),
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
            upstream_key_id: "k1".into(),
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
