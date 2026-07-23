//! Integration tests for console log routes against real temp files.

use std::io::Write;
use std::sync::Arc;

use axum::{
    body::Body,
    http::{Request, StatusCode},
    Router,
};
use tower::ServiceExt;

use crate::log_reader::{local_today_string, log_path, DEFAULT_LOG_PREFIX};
use crate::server::build_console_router;
use crate::state::{DaemonState, LogRuntime};

/// Minimal DaemonState for log handlers only (other fields unused at runtime).
async fn state_with_logs(log: LogRuntime) -> Arc<DaemonState> {
    let data = tempfile::tempdir().expect("data");
    let data_dir = data.path().to_path_buf();
    // Leak so DB paths stay valid for the test process.
    std::mem::forget(data);
    let db_url = format!("sqlite:///{}", data_dir.join("t.db").display());
    let pool = conduit_store::open_db(&db_url).await.expect("db");
    let secret_backend = conduit_secret::build_backend(&data_dir, None).backend;
    let pricing_repo = Arc::new(
        conduit_store::PricingRepo::new(&data_dir)
            .await
            .expect("pricing"),
    );
    let limits_repo = Arc::new(
        conduit_store::LimitsRepo::new(&data_dir)
            .await
            .expect("limits"),
    );
    let routing_table = Arc::new(arc_swap::ArcSwap::from_pointee(
        conduit_router::table::RoutingTable::new(vec![]),
    ));
    let pricing_table = Arc::new(arc_swap::ArcSwap::from_pointee(
        crate::state::PricingMap::new(),
    ));
    let limits_table = Arc::new(arc_swap::ArcSwap::from_pointee(
        crate::state::LimitsMap::new(),
    ));
    let cooldown = Arc::new(conduit_router::ProviderCooldownStore::new());
    let quota_snapshots = Arc::new(conduit_router::UpstreamQuotaStore::new());
    let pipeline = Arc::new(conduit_pipeline::handle::PipelineHandle::new(Arc::new(
        conduit_pipeline::handle::PipelineDeps {
            routing_table: routing_table.clone(),
            secret_fn: Arc::new(|_id: String| {
                Box::pin(async {
                    Err(conduit_ir::error::GatewayError::Internal("unused".into()))
                })
            }),
            pricing_fn: Arc::new(|_k: &str, _m: &str| None),
            quota: Arc::new(conduit_quota::engine::InMemoryQuotaEngine::new(Arc::new(
                |_| Box::pin(async { Ok(()) }),
            ))),
            key_policy_fn: Arc::new(|_tok: String| Box::pin(async { Ok(None) })),
            affinity: Arc::new(conduit_router::AffinityStore::new()),
            pool_cursors: Arc::new(conduit_router::PoolCursorStore::new()),
            cooldown: cooldown.clone(),
            quota_snapshots: quota_snapshots.clone(),
        },
    )));
    Arc::new(DaemonState {
        routing_table,
        pipeline,
        pool,
        secret_backend,
        pricing_repo,
        pricing_table,
        limits_repo,
        limits_table,
        data_dir,
        oauth: Arc::new(crate::oauth::OAuthRuntime::new()),
        proxy_url: None,
        cooldown,
        quota_snapshots,
        version: "test",
        log,
    })
}

async fn oneshot_json(app: Router, uri: &str) -> (StatusCode, serde_json::Value) {
    let res = app
        .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = res.status();
    let body = axum::body::to_bytes(res.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap_or(serde_json::Value::Null);
    (status, v)
}

#[tokio::test]
async fn meta_and_history_serve_known_line_from_temp_file() {
    let log_dir = tempfile::tempdir().unwrap();
    let today = local_today_string();
    let path = log_path(log_dir.path(), DEFAULT_LOG_PREFIX, &today);
    {
        let mut f = std::fs::File::create(&path).unwrap();
        writeln!(
            f,
            "2026-07-24T12:00:00.000Z  INFO conduitd: smoke-marker-xyz unique-token"
        )
        .unwrap();
    }

    let state = state_with_logs(LogRuntime {
        to_file: true,
        dir: log_dir.path().to_path_buf(),
        prefix: DEFAULT_LOG_PREFIX.into(),
        format: "pretty".into(),
        level: "info".into(),
    })
    .await;
    let app = build_console_router(state);

    let (status, meta) = oneshot_json(app.clone(), "/console/logs/meta").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(meta["enabled"], true);
    assert_eq!(meta["today"], today);
    assert!(
        meta["available_dates"]
            .as_array()
            .unwrap()
            .iter()
            .any(|d| d.as_str() == Some(today.as_str())),
        "meta={meta}"
    );

    let (status, page) = oneshot_json(
        app,
        &format!("/console/logs?date={today}&limit=50&q=smoke-marker-xyz"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let lines = page["lines"].as_array().expect("lines");
    assert!(
        lines.iter().any(|l| {
            l["raw"]
                .as_str()
                .map(|r| r.contains("smoke-marker-xyz"))
                .unwrap_or(false)
        }),
        "page={page}"
    );
    drop(log_dir);
}

#[tokio::test]
async fn meta_disabled_when_to_file_false() {
    let log_dir = tempfile::tempdir().unwrap();
    let state = state_with_logs(LogRuntime {
        to_file: false,
        dir: log_dir.path().to_path_buf(),
        prefix: DEFAULT_LOG_PREFIX.into(),
        format: "pretty".into(),
        level: "info".into(),
    })
    .await;
    let app = build_console_router(state);
    let (status, meta) = oneshot_json(app.clone(), "/console/logs/meta").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(meta["enabled"], false);
    assert!(meta["message"].as_str().unwrap_or("").contains("disabled"));

    let (status, body) = oneshot_json(app, "/console/logs?limit=10").await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert!(body["error"].as_str().unwrap_or("").contains("disabled"));
    drop(log_dir);
}
