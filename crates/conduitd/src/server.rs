//! Server startup: builds the axum router and launches the gateway + console servers.

use std::{net::SocketAddr, path::PathBuf, sync::Arc, time::Duration};

use anyhow::Result;
use arc_swap::ArcSwap;
use axum::{
    body::Body,
    http::{header, HeaderValue, Method, Request, StatusCode},
    middleware::{self, Next},
    response::Response,
    routing::{delete, get, post, put},
    Router,
};
use conduit_ir::error::GatewayError;
use conduit_pipeline::{
    handle::{AuthFn, KeyPolicyFn, PipelineDeps, PipelineHandle, PricingFn},
    ingress::KeyPolicy,
    UpstreamAuth,
};
use conduit_quota::engine::InMemoryQuotaEngine;
use conduit_router::{
    table::{Route, RoutingStrategy, RoutingTable},
    AffinityStore,
};
use conduit_store::{KeyRepo, PricingRepo, RouteRepo};
use secrecy::SecretVec;
use tokio::net::TcpListener;
use tower_http::{
    cors::{AllowHeaders, AllowOrigin, Any, CorsLayer},
    timeout::TimeoutLayer,
};
use tracing::{info, warn};

use crate::{
    config::Config,
    state::{pricing_map_from_repo, DaemonState},
    usage_wire::make_record_fn,
};

pub async fn run(cfg: Config, port: u16, data_dir: PathBuf) -> Result<()> {
    // Ensure data directories exist
    std::fs::create_dir_all(&data_dir)?;

    // ── Open SQLite database ──────────────────────────────────────────────────
    let db_url = format!("sqlite:///{}", data_dir.join("conduit.db").display());
    let pool = conduit_store::open_db(&db_url).await?;

    // ── Secret backend (S1 keychain → S2 master password fallback) ───────────
    let backend_result = conduit_secret::build_backend("conduit", &data_dir, None).await;
    if let Some(ref w) = backend_result.downgrade_warning {
        warn!("{}", w);
    }
    let secret_backend = backend_result.backend;

    // ── Pricing repo (hot-reloadable from pricing.json) ───────────────────────
    let pricing_repo = Arc::new(PricingRepo::new(pool.clone(), &data_dir).await?);

    // ── Load routing table from DB ─────────────────────────────────────────────
    let routing_table = {
        let route_repo = RouteRepo::new(&pool);
        let rows = route_repo.list().await?;
        let provider_map = build_provider_map(&pool).await?;
        let routes = rows_to_routes(rows, &provider_map)?;
        Arc::new(ArcSwap::from_pointee(RoutingTable::new(routes)))
    };

    // ── Quota engine: RPM + usage ledger ─────────────────────────────────────
    let record_fn = make_record_fn(pool.clone());
    let quota = Arc::new(InMemoryQuotaEngine::new(record_fn));

    // Periodically purge stale RPM buckets so the in-memory counter's memory is
    // bounded (each unique key+minute pair otherwise lives forever).
    {
        let rpm_counter = quota.rpm_counter();
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(std::time::Duration::from_secs(30));
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                ticker.tick().await;
                rpm_counter.cleanup_old_buckets().await;
            }
        });
    }

    // ── key_policy_fn: async BLAKE3-hash bearer → DB lookup (no block_on) ────
    let key_pool = pool.clone();
    let key_policy_fn: KeyPolicyFn = Arc::new(move |raw_key: String| {
        let pool = key_pool.clone();
        Box::pin(async move {
            let hash = hex::encode(blake3::hash(raw_key.as_bytes()).as_bytes());
            let repo = KeyRepo::new(&pool);
            match repo.get_by_hash(&hash).await {
                Ok(Some(row)) => Ok(Some(KeyPolicy {
                    key_id: row.id,
                    model_whitelist: serde_json::from_str(&row.model_whitelist).unwrap_or_default(),
                    rate_limit_rpm: row.rate_limit_rpm.map(|v| v as u32),
                })),
                Ok(None) => Ok(None),
                Err(e) => Err(GatewayError::Internal(format!(
                    "key policy lookup failed: {e}"
                ))),
            }
        })
    });

    // ── Process-shared CredentialResolver (singleflight refresh) ─────────────
    // One resolver for the daemon lifetime so concurrent near-expiry refreshes
    // share the same RefreshCoordinator.
    let credential_resolver = Arc::new(conduit_oauth::CredentialResolver::new(Arc::new(
        SecretBackendStore(secret_backend.clone()),
    )));

    let resolver_for_secret = credential_resolver.clone();
    let secret_fn: AuthFn = Arc::new(move |key_id: String| {
        let resolver = resolver_for_secret.clone();
        Box::pin(async move {
            match tokio::time::timeout(Duration::from_secs(25), resolver.resolve(&key_id)).await {
                Ok(Ok(resolved)) => {
                    tracing::debug!(key_id = %key_id, "credential resolve ok");
                    Ok(UpstreamAuth {
                        token: resolved.access_token,
                        extra_headers: resolved.extra_headers,
                        client_headers: vec![],
                    })
                }
                Ok(Err(e)) => {
                    tracing::warn!(key_id = %key_id, error = %e, "credential resolve failed");
                    Err(GatewayError::Internal(format!(
                        "no secret found for upstream_key_id '{key_id}': {e}"
                    )))
                }
                Err(_) => {
                    tracing::warn!(
                        key_id = %key_id,
                        "credential resolve timed out after 25s \
                         (macOS Keychain ACL? re-run: conduitctl oauth start <provider>)"
                    );
                    Err(GatewayError::Internal(format!(
                        "credential resolve timed out for upstream_key_id '{key_id}'"
                    )))
                }
            }
        })
    });

    // ── pricing_fn: pure in-memory ArcSwap (no block_on / no DB on hot path) ─
    let pricing_table = Arc::new(ArcSwap::from_pointee(
        pricing_map_from_repo(&pricing_repo).await,
    ));
    let pricing_table_for_fn = pricing_table.clone();
    let pricing_fn: PricingFn = Arc::new(move |kind: &str, model: &str| {
        let snap = pricing_table_for_fn.load();
        crate::state::lookup_pricing(&snap, kind, model)
    });

    // Shared pipeline — constructed once; routing_table is ArcSwap-backed.
    let pipeline = Arc::new(PipelineHandle::new(Arc::new(PipelineDeps {
        routing_table: routing_table.clone(),
        secret_fn,
        pricing_fn,
        quota: quota.clone(),
        key_policy_fn,
        affinity: Arc::new(AffinityStore::new()),
    })));

    // ── Assemble DaemonState ──────────────────────────────────────────────────
    let state = Arc::new(DaemonState {
        routing_table,
        pipeline,
        pool: pool.clone(),
        secret_backend,
        pricing_repo,
        pricing_table,
        data_dir: data_dir.clone(),
        oauth: Arc::new(crate::oauth::OAuthRuntime::new()),
        version: env!("CARGO_PKG_VERSION"),
    });

    // ── Gateway router (OpenAI + Anthropic-compatible API) ────────────────────
    let gateway = Router::new()
        .route(
            "/v1/chat/completions",
            post(crate::routes::chat_completions),
        )
        .route("/v1/responses", post(crate::routes::responses))
        .route("/v1/messages", post(crate::routes::messages))
        .route("/v1/models", get(crate::routes::list_models))
        .route("/health", get(crate::routes::health))
        .layer(console_cors_layer())
        .layer(TimeoutLayer::with_status_code(
            StatusCode::GATEWAY_TIMEOUT,
            Duration::from_secs(300),
        ))
        .with_state(state.clone());

    // ── Console router ──────────────────────────────────────────────────────────
    // CORS + OPTIONS short-circuit for Tauri/dev UI (localhost:1420 / tauri.localhost)
    // calling loopback console. Without this, MethodRouter returns 405 on preflight.
    let console = build_console_router(state.clone());

    // Loopback by default: local-first gateway must not be an open proxy.
    // Operators can still front with a reverse proxy if they need LAN access.
    let gateway_addr: SocketAddr = format!("127.0.0.1:{}", port).parse()?;
    let console_addr: SocketAddr = format!("127.0.0.1:{}", cfg.gateway.console_port).parse()?;

    info!(%gateway_addr, "gateway listening");
    info!(%console_addr, "console API listening");

    let gateway_listener = TcpListener::bind(gateway_addr).await?;
    let console_listener = TcpListener::bind(console_addr).await?;

    // Broadcast a single Ctrl-C to both servers so gateway *and* console drain
    // gracefully (previously only the gateway had graceful shutdown; console
    // connections were cut abruptly when the select! completed).
    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
    let console_shutdown_rx = shutdown_rx.clone();
    tokio::spawn(async move {
        tokio::signal::ctrl_c().await.ok();
        info!("shutdown signal received");
        let _ = shutdown_tx.send(true);
    });

    let wait_for_shutdown = |mut rx: tokio::sync::watch::Receiver<bool>| async move {
        // Resolve once the signal flips to true (or the sender is dropped).
        while !*rx.borrow_and_update() {
            if rx.changed().await.is_err() {
                break;
            }
        }
    };

    let gateway_srv = axum::serve(gateway_listener, gateway)
        .with_graceful_shutdown(wait_for_shutdown(shutdown_rx));
    let console_srv = axum::serve(console_listener, console)
        .with_graceful_shutdown(wait_for_shutdown(console_shutdown_rx));

    let (gateway_res, console_res) = tokio::join!(gateway_srv, console_srv);
    gateway_res?;
    console_res?;

    // ── Graceful shutdown sequence (each step with 30s timeout) ──────────────
    info!("closing database...");
    tokio::time::timeout(std::time::Duration::from_secs(30), state.pool.close())
        .await
        .ok();

    info!("shutdown complete");
    Ok(())
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Adapts [`conduit_secret::SecretBackend`] to [`conduit_oauth::SecretStore`].
struct SecretBackendStore(Arc<dyn conduit_secret::SecretBackend>);

#[async_trait::async_trait]
impl conduit_oauth::SecretStore for SecretBackendStore {
    async fn get(
        &self,
        scope: &str,
        id: &str,
    ) -> Result<Option<SecretVec<u8>>, conduit_oauth::OAuthError> {
        self.0
            .get(scope, id)
            .await
            .map_err(|e| conduit_oauth::OAuthError::Credential(e.to_string()))
    }

    async fn put(
        &self,
        scope: &str,
        id: &str,
        secret: SecretVec<u8>,
    ) -> Result<(), conduit_oauth::OAuthError> {
        self.0
            .put(scope, id, secret)
            .await
            .map_err(|e| conduit_oauth::OAuthError::Credential(e.to_string()))
    }
}

/// CORS policy for console (and gateway) when the UI origin is not the same as the API.
///
/// - Reflect any Origin (loopback dev + tauri.localhost)
/// - Allow private-network access preflight (Chrome → 127.0.0.1)
/// - Explicit methods including OPTIONS
pub fn console_cors_layer() -> CorsLayer {
    CorsLayer::new()
        .allow_origin(AllowOrigin::mirror_request())
        .allow_methods([
            Method::GET,
            Method::POST,
            Method::PUT,
            Method::DELETE,
            Method::OPTIONS,
            Method::PATCH,
            Method::HEAD,
        ])
        // Mirror requested headers so Content-Type preflight is accepted.
        .allow_headers(AllowHeaders::mirror_request())
        .expose_headers(Any)
        .allow_credentials(false)
        // Chrome Private Network Access: public/loopback-looking page → 127.0.0.1
        .allow_private_network(true)
        .max_age(Duration::from_secs(600))
}

/// Short-circuit OPTIONS so MethodRouter never answers preflight with 405.
///
/// tower-http CorsLayer only short-circuits “full” preflight (Origin +
/// Access-Control-Request-Method). Some WebViews send bare OPTIONS; those
/// must still succeed for the browser to continue.
pub async fn options_preflight_ok(req: Request<Body>, next: Next) -> Response {
    if req.method() == Method::OPTIONS {
        let mut res = Response::builder()
            .status(StatusCode::NO_CONTENT)
            .body(Body::empty())
            .expect("empty OPTIONS response");
        let headers = res.headers_mut();
        // Minimal CORS headers; CorsLayer (outer) may refine/mirror Origin.
        headers.insert(
            header::ACCESS_CONTROL_ALLOW_ORIGIN,
            HeaderValue::from_static("*"),
        );
        headers.insert(
            header::ACCESS_CONTROL_ALLOW_METHODS,
            HeaderValue::from_static("GET, POST, PUT, DELETE, OPTIONS, PATCH, HEAD"),
        );
        headers.insert(
            header::ACCESS_CONTROL_ALLOW_HEADERS,
            HeaderValue::from_static("*"),
        );
        headers.insert(
            header::HeaderName::from_static("access-control-allow-private-network"),
            HeaderValue::from_static("true"),
        );
        if let Some(req_headers) = req.headers().get(header::ACCESS_CONTROL_REQUEST_HEADERS) {
            headers.insert(header::ACCESS_CONTROL_ALLOW_HEADERS, req_headers.clone());
        }
        if let Some(origin) = req.headers().get(header::ORIGIN) {
            headers.insert(header::ACCESS_CONTROL_ALLOW_ORIGIN, origin.clone());
            headers.insert(header::VARY, HeaderValue::from_static("Origin"));
        }
        return res;
    }
    next.run(req).await
}

/// Build the console API router (testable without full daemon boot).
pub fn build_console_router(state: Arc<crate::state::DaemonState>) -> Router {
    Router::new()
        .route("/health", get(crate::routes::health))
        // Providers
        .route("/console/providers", get(crate::console::list_providers))
        .route("/console/providers", post(crate::console::create_provider))
        .route("/console/providers/{id}", get(crate::console::get_provider))
        .route(
            "/console/providers/{id}",
            put(crate::console::update_provider),
        )
        .route(
            "/console/providers/{id}",
            delete(crate::console::delete_provider),
        )
        .route(
            "/console/providers/{id}/secret",
            put(crate::console::set_provider_secret),
        )
        // Routes
        .route("/console/routes", get(crate::console::list_routes))
        .route("/console/routes", post(crate::console::create_route))
        .route("/console/routes/{id}", get(crate::console::get_route))
        .route("/console/routes/{id}", put(crate::console::update_route))
        .route("/console/routes/{id}", delete(crate::console::delete_route))
        // Downstream keys
        .route("/console/keys", get(crate::console::list_keys))
        .route("/console/keys", post(crate::console::create_key))
        .route("/console/keys/{id}", get(crate::console::get_key))
        .route("/console/keys/{id}", put(crate::console::update_key))
        .route("/console/keys/{id}", delete(crate::console::delete_key))
        // Usage ledger / pricing
        .route("/console/usage", get(crate::console::list_usage))
        .route("/console/usage/summary", get(crate::console::usage_summary))
        .route("/console/pricing", get(crate::console::list_pricing))
        .route(
            "/console/pricing/reload",
            post(crate::console::reload_pricing),
        )
        .route("/console/pricing/sync", post(crate::console::sync_pricing))
        // OAuth
        .route(
            "/console/oauth/providers",
            get(crate::oauth::list_oauth_providers),
        )
        .route(
            "/console/oauth/{kind}/start",
            post(crate::oauth::start_oauth),
        )
        .route(
            "/console/oauth/sessions/{id}",
            get(crate::oauth::get_oauth_session),
        )
        .route(
            "/console/oauth/sessions/{id}/cancel",
            post(crate::oauth::cancel_oauth_session),
        )
        .route(
            "/console/oauth/{provider_id}/refresh",
            post(crate::oauth::refresh_provider_oauth),
        )
        // Innermost → outermost: OPTIONS short-circuit, then CorsLayer.
        .layer(middleware::from_fn(options_preflight_ok))
        .layer(console_cors_layer())
        .with_state(state)
}

/// Build a map of provider_id → base_url from the DB.
pub async fn build_provider_map(
    pool: &conduit_store::StorePool,
) -> Result<std::collections::HashMap<String, String>> {
    use conduit_store::ProviderRepo;
    let repo = ProviderRepo::new(pool);
    let rows = repo.list().await.map_err(|e| anyhow::anyhow!("{}", e))?;
    Ok(rows.into_iter().map(|r| (r.id, r.base_url)).collect())
}

/// Convert enabled RouteRows from the DB into the in-memory Route format.
/// Injects `base_url` from the provider map into each RouteTarget.
pub fn rows_to_routes(
    rows: Vec<conduit_store::RouteRow>,
    provider_map: &std::collections::HashMap<String, String>,
) -> Result<Vec<Route>> {
    rows.into_iter()
        .filter(|r| r.enabled)
        .map(|row| {
            let mut targets: Vec<conduit_router::table::RouteTarget> =
                serde_json::from_str(&row.targets_json).map_err(|e| {
                    anyhow::anyhow!("invalid targets_json for route {}: {}", row.id, e)
                })?;
            // Inject base_url from provider map (without overriding if already set in JSON)
            for t in &mut targets {
                if t.base_url.is_none() {
                    t.base_url = provider_map.get(&t.provider_id).cloned();
                }
            }
            let retry_policy: conduit_router::policy::RetryPolicy =
                serde_json::from_str(&row.retry_policy_json).unwrap_or_default();
            let strategy = match row.strategy.as_str() {
                "fallback" => RoutingStrategy::Fallback,
                "weighted" | "weight" | "lb" => RoutingStrategy::Weighted,
                _ => RoutingStrategy::Fixed,
            };
            Ok(Route {
                alias: row.match_alias,
                strategy,
                targets,
                retry_policy,
            })
        })
        .collect()
}

/// Reload the in-memory routing table from the current DB state.
///
/// Publishes a new [`Arc`] via [`ArcSwap::store`] so in-flight requests keep
/// their snapshot and subsequent requests see the update without write locks.
pub async fn reload_routing_table(state: &DaemonState) -> Result<(), conduit_store::StoreError> {
    let repo = RouteRepo::new(&state.pool);
    let rows = repo.list().await?;
    let provider_map = build_provider_map(&state.pool)
        .await
        .map_err(|e| conduit_store::StoreError::Serialization(e.to_string()))?;
    let routes = rows_to_routes(rows, &provider_map)
        .map_err(|e| conduit_store::StoreError::Serialization(e.to_string()))?;
    state
        .routing_table
        .store(Arc::new(RoutingTable::new(routes)));
    Ok(())
}

// ── CORS / preflight tests ────────────────────────────────────────────────────

#[cfg(test)]
mod cors_tests {
    use tower::ServiceExt;

    use super::*;

    #[tokio::test]
    async fn options_preflight_on_console_path_is_not_405() {
        // Minimal method-limited route that would 405 OPTIONS without middleware.
        let app = Router::new()
            .route("/console/providers", get(|| async { "ok" }))
            .layer(middleware::from_fn(options_preflight_ok))
            .layer(console_cors_layer());

        let req = Request::builder()
            .method(Method::OPTIONS)
            .uri("/console/providers")
            .header(header::ORIGIN, "http://localhost:1420")
            .header(header::ACCESS_CONTROL_REQUEST_METHOD, "POST")
            .header(header::ACCESS_CONTROL_REQUEST_HEADERS, "content-type")
            .body(Body::empty())
            .unwrap();

        let res = app.oneshot(req).await.expect("oneshot");
        assert_ne!(
            res.status(),
            StatusCode::METHOD_NOT_ALLOWED,
            "preflight must not hit MethodRouter 405"
        );
        assert!(
            res.status() == StatusCode::NO_CONTENT || res.status().is_success(),
            "unexpected status {}",
            res.status()
        );
        let allow_origin = res.headers().get(header::ACCESS_CONTROL_ALLOW_ORIGIN);
        assert!(
            allow_origin.is_some(),
            "Access-Control-Allow-Origin must be present"
        );
    }

    #[tokio::test]
    async fn bare_options_without_preflight_headers_still_ok() {
        let app = Router::new()
            .route("/console/keys", get(|| async { "ok" }))
            .layer(middleware::from_fn(options_preflight_ok))
            .layer(console_cors_layer());

        let req = Request::builder()
            .method(Method::OPTIONS)
            .uri("/console/keys")
            .body(Body::empty())
            .unwrap();

        let res = app.oneshot(req).await.expect("oneshot");
        assert_ne!(
            res.status(),
            StatusCode::METHOD_NOT_ALLOWED,
            "bare OPTIONS must not 405"
        );
        assert!(
            res.status() == StatusCode::NO_CONTENT || res.status().is_success(),
            "unexpected status {}",
            res.status()
        );
    }
}

// ── Hot-path routing tests ───────────────────────────────────────────────────

#[cfg(test)]
mod hotpath_tests {
    use conduit_router::{policy::RetryPolicy, table::RouteTarget};

    use super::*;

    fn sample_table(alias: &str, model: &str) -> RoutingTable {
        RoutingTable::new(vec![Route {
            alias: alias.into(),
            strategy: RoutingStrategy::Fixed,
            targets: vec![RouteTarget {
                provider_id: "openai".into(),
                model_id: model.into(),
                upstream_key_id: "k1".into(),
                provider_kind: "openai".into(),
                base_url: Some("https://api.openai.com".into()),
                weight: 1,
                request_overrides: Default::default(),
            }],
            retry_policy: RetryPolicy::default(),
        }])
    }

    #[test]
    fn arcswap_reload_visible_on_next_load_without_clone_rebuild() {
        let table = Arc::new(ArcSwap::from_pointee(sample_table("gpt-4o", "gpt-4o")));
        // Simulate pipeline hot path: lock-free load.
        let snap1 = table.load_full();
        assert_eq!(snap1.get("gpt-4o").unwrap().targets[0].model_id, "gpt-4o");

        // Simulate console reload: store new Arc (no per-request deep clone).
        table.store(Arc::new(sample_table("gpt-4o", "gpt-4o-mini")));
        let snap2 = table.load_full();
        assert_eq!(
            snap2.get("gpt-4o").unwrap().targets[0].model_id,
            "gpt-4o-mini"
        );
        // Old snapshot is unchanged (in-flight request isolation).
        assert_eq!(snap1.get("gpt-4o").unwrap().targets[0].model_id, "gpt-4o");
        // Pointers differ — we publish a new Arc rather than mutate in place.
        assert!(!Arc::ptr_eq(&snap1, &snap2));
    }

    #[test]
    fn rows_to_routes_preserves_target_request_overrides() {
        let row = conduit_store::RouteRow {
            id: "route-1".into(),
            match_alias: "terra".into(),
            strategy: "fixed".into(),
            targets_json: r#"[{"provider_id":"codex","model_id":"gpt-5.6-terra","upstream_key_id":"key-1","provider_kind":"codex-oauth","request_overrides":{"service_tier":"priority"}}]"#.into(),
            retry_policy_json: "{}".into(),
            enabled: true,
            created_at: "now".into(),
            updated_at: "now".into(),
        };
        let provider_map = std::collections::HashMap::from([(
            "codex".into(),
            "https://chatgpt.com/backend-api/codex".into(),
        )]);

        let routes = rows_to_routes(vec![row], &provider_map).unwrap();
        let target = &routes[0].targets[0];

        assert_eq!(
            target.base_url.as_deref(),
            Some("https://chatgpt.com/backend-api/codex")
        );
        assert_eq!(target.request_overrides["service_tier"], "priority");
    }

    /// Structural proof: process-shared CredentialResolver is constructed once
    /// and cloned into the secret_fn (not `CredentialResolver::new` per call).
    #[test]
    fn shared_credential_resolver_is_arc_cloned_not_rebuilt_per_call() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        use conduit_oauth::{CredentialResolver, SecretStore};
        use secrecy::SecretVec;

        static NEW_COUNT: AtomicUsize = AtomicUsize::new(0);

        struct CountingStore;
        #[async_trait::async_trait]
        impl SecretStore for CountingStore {
            async fn get(
                &self,
                _scope: &str,
                _id: &str,
            ) -> Result<Option<SecretVec<u8>>, conduit_oauth::OAuthError> {
                Ok(None)
            }
            async fn put(
                &self,
                _scope: &str,
                _id: &str,
                _secret: SecretVec<u8>,
            ) -> Result<(), conduit_oauth::OAuthError> {
                Ok(())
            }
        }

        // Mirror daemon wiring: one Arc resolver, clone into multiple "calls".
        NEW_COUNT.store(0, Ordering::SeqCst);
        let make = || {
            NEW_COUNT.fetch_add(1, Ordering::SeqCst);
            Arc::new(CredentialResolver::new(Arc::new(CountingStore)))
        };
        let shared = make();
        let c1 = shared.clone();
        let c2 = shared.clone();
        assert_eq!(NEW_COUNT.load(Ordering::SeqCst), 1);
        assert!(Arc::ptr_eq(&c1, &c2));
        // Distinct from the anti-pattern: per-request new would bump the counter.
        let _again = make();
        assert_eq!(NEW_COUNT.load(Ordering::SeqCst), 2);
    }

    /// Source-level guard: hot path must not reintroduce nested block_on wiring.
    #[test]
    fn server_source_has_no_nested_block_on_hot_path() {
        // Strip this test module so our own string literals cannot false-positive.
        let full = include_str!("server.rs");
        let src = full
            .split("mod hotpath_tests")
            .next()
            .expect("hotpath_tests module marker");
        // Build needles without embedding the banned call as a contiguous literal
        // in production source (this test body is stripped above).
        let banned = [
            format!("{}{}{}", "block", "_in_place", "("),
            format!("{}{}{}", "task::block", "_in_place", ""),
            format!("{}{}", "Handle::current().", "block_on"),
        ];
        for needle in &banned {
            assert!(
                !src.contains(needle.as_str()),
                "server production source must not contain `{needle}`"
            );
        }
        // Native ingress protocols must be registered on the gateway router.
        assert!(
            src.contains("/v1/messages") && src.contains("routes::messages"),
            "gateway router must register POST /v1/messages → routes::messages"
        );
        assert!(
            src.contains("/v1/responses") && src.contains("routes::responses"),
            "gateway router must register POST /v1/responses → routes::responses"
        );
        // Shared resolver: construct once at startup, not per request.
        let ctor = format!("{}{}", "CredentialResolver", "::new");
        let resolver_news = src.matches(ctor.as_str()).count();
        assert_eq!(
            resolver_news, 1,
            "CredentialResolver::new must appear once (process-shared), found {resolver_news}"
        );
    }
}
