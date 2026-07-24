//! PipelineHandle: main entry point for running requests through the L2-L7 pipeline.

mod compact;
mod execute;
mod support;

use std::{future::Future, pin::Pin, sync::Arc};

use conduit_ir::{
    canonical::{CanonicalChatRequest, CanonicalChatResponse, CanonicalChunk},
    error::{GatewayError, ProviderError},
};
use conduit_router::{
    table::RoutingTable, AffinityStore, PoolCursorStore, ProviderCooldownStore, UpstreamQuotaStore,
};
use futures::stream::BoxStream;
use tracing::debug;

use super::{
    context::{IngressWire, PipelineContext},
    ingress::KeyPolicy,
    provider::UpstreamAuth,
    stage::route_request_with_skip,
};

pub use support::resolve_session_id;

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
    pub(crate) deps: Arc<PipelineDeps>,
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

        // Ensure every log under this request (including auth/oauth/upstream
        // paths that don't take request_id as a parameter) can be grepped by rid.
        tracing::Span::current().record("request_id", tracing::field::display(&ctx.request_id));

        debug!(
            request_id = %ctx.request_id,
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

}

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

    use super::support::request_for_upstream;
    use super::*;
    use super::super::context::{IngressWire, ResolvedProvider};
    use super::super::egress;
    use super::super::provider::UpstreamAuth;
    use conduit_ir::error::GatewayError;
    use conduit_ir::wire_format::WireFormat;
    use conduit_router::{AffinityStore, PoolCursorStore, ProviderCooldownStore, UpstreamQuotaStore};

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
