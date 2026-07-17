//! Admin API handlers — CRUD for providers, routes, downstream keys, usage, pricing.

use std::sync::Arc;

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::IntoResponse,
    Json,
};
use chrono::Utc;
use conduit_store::{
    schema::{DownstreamKeyRow, ProviderRow, RouteRow},
    KeyRepo, ProviderRepo, RouteRepo, UsageRepo,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use ulid::Ulid;

use crate::{server::reload_routing_table, state::DaemonState};

// ── Error helper ─────────────────────────────────────────────────────────────

fn err(status: StatusCode, msg: impl Into<String>) -> (StatusCode, Json<Value>) {
    (status, Json(json!({"error": msg.into()})))
}

fn internal(e: impl std::fmt::Display) -> (StatusCode, Json<Value>) {
    err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
}

// ═══════════════════════════════════════════════════════════════════════════════
// Providers
// ═══════════════════════════════════════════════════════════════════════════════

/// POST body for creating a provider.
#[derive(Debug, Deserialize)]
pub struct CreateProviderBody {
    pub name: String,
    pub kind: String,
    pub base_url: String,
    /// Optional plaintext API key — stored immediately via the secret backend.
    /// If omitted, the secret must be set separately via PUT /admin/providers/{id}/secret.
    pub api_key: Option<String>,
}

/// PUT body for updating a provider.
#[derive(Debug, Deserialize)]
pub struct UpdateProviderBody {
    pub name: Option<String>,
    pub base_url: Option<String>,
}

pub async fn list_providers(State(state): State<Arc<DaemonState>>) -> impl IntoResponse {
    let repo = ProviderRepo::new(&state.pool);
    match repo.list().await {
        Ok(rows) => (StatusCode::OK, Json(json!(rows))).into_response(),
        Err(e) => internal(e).into_response(),
    }
}

pub async fn create_provider(
    State(state): State<Arc<DaemonState>>,
    Json(body): Json<CreateProviderBody>,
) -> impl IntoResponse {
    if body.name.is_empty() || body.kind.is_empty() || body.base_url.is_empty() {
        return err(
            StatusCode::BAD_REQUEST,
            "name, kind, and base_url are required",
        )
        .into_response();
    }
    let id = Ulid::new().to_string();
    let now = Utc::now().to_rfc3339();
    let upstream_key_ref = format!("keyring://upstream_key/{}", id);

    let row = ProviderRow {
        id: id.clone(),
        name: body.name,
        kind: body.kind,
        base_url: body.base_url,
        upstream_key_ref: upstream_key_ref.clone(),
        created_at: now.clone(),
        updated_at: now,
    };

    let repo = ProviderRepo::new(&state.pool);
    if let Err(e) = repo.insert(&row).await {
        return internal(e).into_response();
    }

    // Store API key secret if provided
    if let Some(api_key) = body.api_key {
        let bytes: Vec<u8> = api_key.into_bytes();
        let secret = secrecy::SecretVec::new(bytes);
        if let Err(e) = state.secret_backend.put("upstream_key", &id, secret).await {
            // Roll back: delete the provider row we just inserted
            let _ = repo.delete(&id).await;
            return err(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("failed to store secret: {}", e),
            )
            .into_response();
        }
    }

    (StatusCode::CREATED, Json(json!(row))).into_response()
}

pub async fn get_provider(
    State(state): State<Arc<DaemonState>>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    let repo = ProviderRepo::new(&state.pool);
    match repo.get(&id).await {
        Ok(Some(row)) => (StatusCode::OK, Json(json!(row))).into_response(),
        Ok(None) => err(StatusCode::NOT_FOUND, "provider not found").into_response(),
        Err(e) => internal(e).into_response(),
    }
}

pub async fn update_provider(
    State(state): State<Arc<DaemonState>>,
    Path(id): Path<String>,
    Json(body): Json<UpdateProviderBody>,
) -> impl IntoResponse {
    let repo = ProviderRepo::new(&state.pool);
    let existing = match repo.get(&id).await {
        Ok(Some(r)) => r,
        Ok(None) => return err(StatusCode::NOT_FOUND, "provider not found").into_response(),
        Err(e) => return internal(e).into_response(),
    };

    let updated = ProviderRow {
        name: body.name.unwrap_or(existing.name),
        base_url: body.base_url.unwrap_or(existing.base_url),
        updated_at: Utc::now().to_rfc3339(),
        ..existing
    };

    match repo.update(&updated).await {
        Ok(()) => (StatusCode::OK, Json(json!(updated))).into_response(),
        Err(e) => internal(e).into_response(),
    }
}

pub async fn delete_provider(
    State(state): State<Arc<DaemonState>>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    let repo = ProviderRepo::new(&state.pool);
    match repo.delete(&id).await {
        Ok(()) => {
            // Best-effort: remove the secret too
            let _ = state.secret_backend.delete("upstream_key", &id).await;
            StatusCode::NO_CONTENT.into_response()
        }
        Err(e) => internal(e).into_response(),
    }
}

/// PUT /admin/providers/{id}/secret — store or rotate the upstream API key.
#[derive(Debug, Deserialize)]
pub struct SetSecretBody {
    pub api_key: String,
}

pub async fn set_provider_secret(
    State(state): State<Arc<DaemonState>>,
    Path(id): Path<String>,
    Json(body): Json<SetSecretBody>,
) -> impl IntoResponse {
    // Verify provider exists
    let repo = ProviderRepo::new(&state.pool);
    match repo.get(&id).await {
        Ok(None) => return err(StatusCode::NOT_FOUND, "provider not found").into_response(),
        Err(e) => return internal(e).into_response(),
        Ok(Some(_)) => {}
    }

    let bytes: Vec<u8> = body.api_key.into_bytes();
    let secret = secrecy::SecretVec::new(bytes);
    match state.secret_backend.put("upstream_key", &id, secret).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => err(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("failed to store secret: {}", e),
        )
        .into_response(),
    }
}

// ═══════════════════════════════════════════════════════════════════════════════
// Routes
// ═══════════════════════════════════════════════════════════════════════════════

#[derive(Debug, Deserialize)]
pub struct CreateRouteBody {
    pub match_alias: String,
    /// "fixed" or "fallback"
    pub strategy: String,
    /// JSON array of RouteTarget objects
    pub targets: Value,
    /// Optional JSON RetryPolicy object
    pub retry_policy: Option<Value>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateRouteBody {
    pub match_alias: Option<String>,
    pub strategy: Option<String>,
    pub targets: Option<Value>,
    pub retry_policy: Option<Value>,
    pub enabled: Option<bool>,
}

pub async fn list_routes(State(state): State<Arc<DaemonState>>) -> impl IntoResponse {
    let repo = RouteRepo::new(&state.pool);
    match repo.list_all().await {
        Ok(rows) => (StatusCode::OK, Json(json!(rows))).into_response(),
        Err(e) => internal(e).into_response(),
    }
}

pub async fn create_route(
    State(state): State<Arc<DaemonState>>,
    Json(body): Json<CreateRouteBody>,
) -> impl IntoResponse {
    if body.match_alias.is_empty() {
        return err(StatusCode::BAD_REQUEST, "match_alias is required").into_response();
    }
    let targets_json = body.targets.to_string();
    let retry_policy_json = body.retry_policy.map(|v| v.to_string()).unwrap_or_else(|| {
        r#"{"max_retries":2,"base_delay_ms":500,"retryable_statuses":[429,500,502,503,504]}"#
            .to_string()
    });

    let id = Ulid::new().to_string();
    let now = Utc::now().to_rfc3339();
    let row = RouteRow {
        id: id.clone(),
        match_alias: body.match_alias,
        strategy: body.strategy,
        targets_json,
        retry_policy_json,
        enabled: true,
        created_at: now.clone(),
        updated_at: now,
    };

    let repo = RouteRepo::new(&state.pool);
    if let Err(e) = repo.insert(&row).await {
        return internal(e).into_response();
    }

    if let Err(e) = reload_routing_table(&state).await {
        tracing::warn!("route created but routing table reload failed: {}", e);
    }

    (StatusCode::CREATED, Json(json!(row))).into_response()
}

pub async fn get_route(
    State(state): State<Arc<DaemonState>>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    let repo = RouteRepo::new(&state.pool);
    match repo.get(&id).await {
        Ok(Some(row)) => (StatusCode::OK, Json(json!(row))).into_response(),
        Ok(None) => err(StatusCode::NOT_FOUND, "route not found").into_response(),
        Err(e) => internal(e).into_response(),
    }
}

pub async fn update_route(
    State(state): State<Arc<DaemonState>>,
    Path(id): Path<String>,
    Json(body): Json<UpdateRouteBody>,
) -> impl IntoResponse {
    let repo = RouteRepo::new(&state.pool);
    let existing = match repo.get(&id).await {
        Ok(Some(r)) => r,
        Ok(None) => return err(StatusCode::NOT_FOUND, "route not found").into_response(),
        Err(e) => return internal(e).into_response(),
    };

    let updated = RouteRow {
        match_alias: body.match_alias.unwrap_or(existing.match_alias),
        strategy: body.strategy.unwrap_or(existing.strategy),
        targets_json: body
            .targets
            .map(|v| v.to_string())
            .unwrap_or(existing.targets_json),
        retry_policy_json: body
            .retry_policy
            .map(|v| v.to_string())
            .unwrap_or(existing.retry_policy_json),
        enabled: body.enabled.unwrap_or(existing.enabled),
        updated_at: Utc::now().to_rfc3339(),
        ..existing
    };

    if let Err(e) = repo.upsert(&updated).await {
        return internal(e).into_response();
    }

    if let Err(e) = reload_routing_table(&state).await {
        tracing::warn!("route updated but routing table reload failed: {}", e);
    }

    (StatusCode::OK, Json(json!(updated))).into_response()
}

pub async fn delete_route(
    State(state): State<Arc<DaemonState>>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    let repo = RouteRepo::new(&state.pool);
    match repo.delete(&id).await {
        Ok(()) => {
            if let Err(e) = reload_routing_table(&state).await {
                tracing::warn!("route deleted but routing table reload failed: {}", e);
            }
            StatusCode::NO_CONTENT.into_response()
        }
        Err(e) => internal(e).into_response(),
    }
}

// ═══════════════════════════════════════════════════════════════════════════════
// Downstream Keys
// ═══════════════════════════════════════════════════════════════════════════════

#[derive(Debug, Serialize, Deserialize)]
pub struct KeyCreateResponse {
    pub id: String,
    /// The raw bearer token — shown ONCE and never again.
    pub key: String,
    pub name: String,
    pub model_whitelist: Vec<String>,
    pub rate_limit_rpm: Option<i64>,
    pub created_at: String,
}

#[derive(Debug, Deserialize)]
pub struct CreateKeyBody {
    pub name: String,
    pub model_whitelist: Option<Vec<String>>,
    pub rate_limit_rpm: Option<i64>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateKeyBody {
    pub name: Option<String>,
    pub model_whitelist: Option<Vec<String>>,
    pub rate_limit_rpm: Option<i64>,
    pub enabled: Option<bool>,
}

pub async fn list_keys(State(state): State<Arc<DaemonState>>) -> impl IntoResponse {
    let repo = KeyRepo::new(&state.pool);
    match repo.list().await {
        Ok(rows) => {
            // Strip key_hash from the response
            let safe: Vec<Value> = rows
                .into_iter()
                .map(|r| {
                    json!({
                        "id": r.id,
                        "name": r.name,
                        "model_whitelist": serde_json::from_str::<Value>(&r.model_whitelist).unwrap_or(json!([])),
                        "rate_limit_rpm": r.rate_limit_rpm,
                        "enabled": r.enabled,
                        "created_at": r.created_at,
                        "updated_at": r.updated_at,
                    })
                })
                .collect();
            (StatusCode::OK, Json(json!(safe))).into_response()
        }
        Err(e) => internal(e).into_response(),
    }
}

pub async fn create_key(
    State(state): State<Arc<DaemonState>>,
    Json(body): Json<CreateKeyBody>,
) -> impl IntoResponse {
    if body.name.is_empty() {
        return err(StatusCode::BAD_REQUEST, "name is required").into_response();
    }

    // Generate a cryptographically random key: "ck_" + 32-byte random hex
    let rand_bytes = {
        let mut buf = [0u8; 32];
        // Use blake3's keyed hash on a random ULID as entropy source
        let ulid_bytes = Ulid::new().to_string();
        let ts_bytes = Utc::now()
            .timestamp_nanos_opt()
            .unwrap_or_default()
            .to_le_bytes();
        let mut combined = ulid_bytes.into_bytes();
        combined.extend_from_slice(&ts_bytes);
        let hash = blake3::hash(&combined);
        buf.copy_from_slice(hash.as_bytes());
        buf
    };
    let raw_key = format!("ck_{}", hex::encode(rand_bytes));
    let key_hash = hex::encode(blake3::hash(raw_key.as_bytes()).as_bytes());

    let id = Ulid::new().to_string();
    let now = Utc::now().to_rfc3339();
    let whitelist = body.model_whitelist.unwrap_or_default();
    let whitelist_json = serde_json::to_string(&whitelist).unwrap_or_else(|_| "[]".to_string());

    let row = DownstreamKeyRow {
        id: id.clone(),
        name: body.name.clone(),
        key_hash,
        model_whitelist: whitelist_json,
        // Column retained for schema compatibility; budget limits removed.
        monthly_budget_usd: None,
        rate_limit_rpm: body.rate_limit_rpm,
        enabled: true,
        created_at: now.clone(),
        updated_at: now.clone(),
    };

    let repo = KeyRepo::new(&state.pool);
    match repo.insert(&row).await {
        Ok(()) => {
            let resp = KeyCreateResponse {
                id,
                key: raw_key,
                name: body.name,
                model_whitelist: whitelist,
                rate_limit_rpm: body.rate_limit_rpm,
                created_at: now,
            };
            (StatusCode::CREATED, Json(json!(resp))).into_response()
        }
        Err(e) => internal(e).into_response(),
    }
}

pub async fn get_key(
    State(state): State<Arc<DaemonState>>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    let repo = KeyRepo::new(&state.pool);
    match repo.get(&id).await {
        Ok(Some(row)) => {
            let safe = json!({
                "id": row.id,
                "name": row.name,
                "model_whitelist": serde_json::from_str::<Value>(&row.model_whitelist).unwrap_or(json!([])),
                "rate_limit_rpm": row.rate_limit_rpm,
                "enabled": row.enabled,
                "created_at": row.created_at,
                "updated_at": row.updated_at,
            });
            (StatusCode::OK, Json(safe)).into_response()
        }
        Ok(None) => err(StatusCode::NOT_FOUND, "key not found").into_response(),
        Err(e) => internal(e).into_response(),
    }
}

pub async fn update_key(
    State(state): State<Arc<DaemonState>>,
    Path(id): Path<String>,
    Json(body): Json<UpdateKeyBody>,
) -> impl IntoResponse {
    let repo = KeyRepo::new(&state.pool);
    let existing = match repo.get(&id).await {
        Ok(Some(r)) => r,
        Ok(None) => return err(StatusCode::NOT_FOUND, "key not found").into_response(),
        Err(e) => return internal(e).into_response(),
    };

    let now = Utc::now().to_rfc3339();
    let updated = DownstreamKeyRow {
        name: body.name.unwrap_or(existing.name),
        model_whitelist: body
            .model_whitelist
            .map(|v| serde_json::to_string(&v).unwrap_or_else(|_| "[]".to_string()))
            .unwrap_or(existing.model_whitelist),
        monthly_budget_usd: None,
        rate_limit_rpm: if body.rate_limit_rpm.is_some() {
            body.rate_limit_rpm
        } else {
            existing.rate_limit_rpm
        },
        enabled: body.enabled.unwrap_or(existing.enabled),
        updated_at: now,
        ..existing
    };

    if let Err(e) = repo.set_enabled(&updated.id, updated.enabled).await {
        return internal(e).into_response();
    }
    if let Err(e) = repo
        .set_rate_limit_rpm(&updated.id, updated.rate_limit_rpm)
        .await
    {
        return internal(e).into_response();
    }

    let safe = json!({
        "id": updated.id,
        "name": updated.name,
        "model_whitelist": serde_json::from_str::<Value>(&updated.model_whitelist).unwrap_or(json!([])),
        "rate_limit_rpm": updated.rate_limit_rpm,
        "enabled": updated.enabled,
        "updated_at": updated.updated_at,
    });
    (StatusCode::OK, Json(safe)).into_response()
}

pub async fn delete_key(
    State(state): State<Arc<DaemonState>>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    let repo = KeyRepo::new(&state.pool);
    match repo.delete(&id).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => internal(e).into_response(),
    }
}

// ═══════════════════════════════════════════════════════════════════════════════
// Settings (runtime toggles)
// ═══════════════════════════════════════════════════════════════════════════════

/// GET /admin/settings — current operator settings + effective values.
pub async fn get_settings(State(state): State<Arc<DaemonState>>) -> impl IntoResponse {
    let runtime = state
        .runtime_settings
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone();
    (
        StatusCode::OK,
        Json(json!({
            "trace": {
                "enabled": state.trace_sink.is_enabled(),
                "config_default": state.trace_config.enabled,
                "runtime_override": runtime.trace_enabled,
                "max_segment_mb": state.trace_config.max_segment_mb,
                "max_db_size_mb": state.trace_config.max_db_size_mb,
                "retention_days": state.trace_config.retention_days,
            }
        })),
    )
        .into_response()
}

#[derive(Debug, Deserialize)]
pub struct UpdateSettingsBody {
    pub trace: Option<UpdateTraceSettingsBody>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateTraceSettingsBody {
    pub enabled: Option<bool>,
}

/// PUT /admin/settings — update runtime settings (persisted to settings.json).
pub async fn update_settings(
    State(state): State<Arc<DaemonState>>,
    Json(body): Json<UpdateSettingsBody>,
) -> impl IntoResponse {
    let mut changed = false;
    let mut runtime = state
        .runtime_settings
        .lock()
        .unwrap_or_else(|e| e.into_inner());

    if let Some(trace) = body.trace {
        if let Some(enabled) = trace.enabled {
            if runtime.trace_enabled != Some(enabled) {
                runtime.trace_enabled = Some(enabled);
                changed = true;
            }
            state.trace_sink.set_enabled(enabled);
            tracing::info!(enabled, "trace recording toggled via admin API");
        }
    }

    if changed {
        if let Err(e) = runtime.save(&state.data_dir) {
            return internal(format!("failed to persist settings: {e}")).into_response();
        }
    }

    let snap = runtime.clone();
    drop(runtime);

    (
        StatusCode::OK,
        Json(json!({
            "trace": {
                "enabled": state.trace_sink.is_enabled(),
                "config_default": state.trace_config.enabled,
                "runtime_override": snap.trace_enabled,
                "max_segment_mb": state.trace_config.max_segment_mb,
                "max_db_size_mb": state.trace_config.max_db_size_mb,
                "retention_days": state.trace_config.retention_days,
            }
        })),
    )
        .into_response()
}

// ═══════════════════════════════════════════════════════════════════════════════
// Usage ledger (per-request consumption; independent of traces)
// ═══════════════════════════════════════════════════════════════════════════════

#[derive(Debug, Deserialize)]
pub struct ListUsageQuery {
    #[serde(default = "default_usage_limit")]
    pub limit: usize,
    pub key_id: Option<String>,
}

fn default_usage_limit() -> usize {
    50
}

/// GET /admin/usage — recent per-request consumption rows.
pub async fn list_usage(
    State(state): State<Arc<DaemonState>>,
    Query(q): Query<ListUsageQuery>,
) -> impl IntoResponse {
    let repo = UsageRepo::new(&state.pool);
    match repo.list(q.limit, q.key_id.as_deref()).await {
        Ok(rows) => (StatusCode::OK, Json(json!({ "entries": rows }))).into_response(),
        Err(e) => internal(e).into_response(),
    }
}

#[derive(Debug, Deserialize)]
pub struct UsageSummaryQuery {
    /// `YYYY-MM`; defaults to current UTC month.
    pub period: Option<String>,
}

/// GET /admin/usage/summary — aggregate spend by key for a calendar month.
pub async fn usage_summary(
    State(state): State<Arc<DaemonState>>,
    Query(q): Query<UsageSummaryQuery>,
) -> impl IntoResponse {
    use chrono::Datelike;
    let now = Utc::now();
    let period = q
        .period
        .unwrap_or_else(|| format!("{:04}-{:02}", now.year(), now.month()));
    let repo = UsageRepo::new(&state.pool);
    match repo.summary_period(&period).await {
        Ok(entries) => {
            let total_usd: f64 = entries.iter().map(|e| e.total_usd).sum();
            let request_count: u64 = entries.iter().map(|e| e.request_count).sum();
            (
                StatusCode::OK,
                Json(json!({
                    "period": period,
                    "total_usd": total_usd,
                    "request_count": request_count,
                    "entries": entries.iter().map(|e| json!({
                        "downstream_key_id": e.downstream_key_id,
                        "request_count": e.request_count,
                        "total_usd": e.total_usd,
                        "prompt_tokens": e.prompt_tokens,
                        "completion_tokens": e.completion_tokens,
                        "total_tokens": e.total_tokens,
                    })).collect::<Vec<_>>(),
                })),
            )
                .into_response()
        }
        Err(e) => internal(e).into_response(),
    }
}

// ═══════════════════════════════════════════════════════════════════════════════
// Pricing
// ═══════════════════════════════════════════════════════════════════════════════

pub async fn list_pricing(State(state): State<Arc<DaemonState>>) -> impl IntoResponse {
    let rows = state.pricing_repo.all().await;
    (StatusCode::OK, Json(json!(rows))).into_response()
}

pub async fn reload_pricing(State(state): State<Arc<DaemonState>>) -> impl IntoResponse {
    match state.pricing_repo.reload(&state.data_dir).await {
        Ok(()) => {
            // Keep pipeline hot-path pricing map in sync (sync ArcSwap lookup).
            let map = crate::state::pricing_map_from_repo(&state.pricing_repo).await;
            state.pricing_table.store(std::sync::Arc::new(map));
            (StatusCode::OK, Json(json!({"status": "reloaded"}))).into_response()
        }
        Err(e) => internal(e).into_response(),
    }
}

/// Optional body for `POST /admin/pricing/sync`.
#[derive(Debug, Default, Deserialize)]
pub struct SyncPricingBody {
    /// Override LiteLLM cost-map URL (default: GitHub raw main).
    #[serde(default)]
    pub url: Option<String>,
}

/// POST /admin/pricing/sync — fetch LiteLLM price map, convert, cache, reload.
///
/// Offline-friendly: never runs automatically at boot; only on explicit request.
/// Writes `{data_dir}/pricing.litellm.json`. Operator `pricing.json` still wins
/// on merge conflicts.
pub async fn sync_pricing(
    State(state): State<Arc<DaemonState>>,
    body: Option<Json<SyncPricingBody>>,
) -> impl IntoResponse {
    let url = body
        .and_then(|Json(b)| b.url)
        .filter(|u| !u.trim().is_empty())
        .unwrap_or_else(|| conduit_store::DEFAULT_LITELLM_PRICING_URL.to_string());

    let client = match reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(60))
        .user_agent(concat!("conduitd/", env!("CARGO_PKG_VERSION")))
        .build()
    {
        Ok(c) => c,
        Err(e) => return internal(e).into_response(),
    };

    let response = match client.get(&url).send().await {
        Ok(r) => r,
        Err(e) => {
            return err(
                StatusCode::BAD_GATEWAY,
                format!("failed to fetch pricing source: {e}"),
            )
            .into_response();
        }
    };

    if !response.status().is_success() {
        return err(
            StatusCode::BAD_GATEWAY,
            format!(
                "pricing source returned HTTP {}: {url}",
                response.status().as_u16()
            ),
        )
        .into_response();
    }

    let text = match response.text().await {
        Ok(t) => t,
        Err(e) => {
            return err(
                StatusCode::BAD_GATEWAY,
                format!("failed to read pricing source body: {e}"),
            )
            .into_response();
        }
    };

    let sync_date = Utc::now().format("%Y-%m-%d").to_string();
    match state
        .pricing_repo
        .apply_litellm_json(&state.data_dir, &text, &sync_date)
        .await
    {
        Ok((total_rows, source_models, skipped)) => {
            let map = crate::state::pricing_map_from_repo(&state.pricing_repo).await;
            state.pricing_table.store(std::sync::Arc::new(map));
            (
                StatusCode::OK,
                Json(json!({
                    "status": "synced",
                    "source": "litellm",
                    "url": url,
                    "sync_date": sync_date,
                    "source_models": source_models,
                    "skipped": skipped,
                    "total_rows": total_rows,
                })),
            )
                .into_response()
        }
        Err(e) => internal(e).into_response(),
    }
}

// ═══════════════════════════════════════════════════════════════════════════════
// Traces
// ═══════════════════════════════════════════════════════════════════════════════

#[derive(Debug, Deserialize)]
pub struct ListTracesQuery {
    #[serde(default = "default_trace_limit")]
    pub limit: usize,
    /// When true, list every event; default lists only request_received anchors.
    #[serde(default)]
    pub all: bool,
}

fn default_trace_limit() -> usize {
    20
}

/// GET /admin/traces — recent index rows (one row per request by default).
///
/// Defaults to `kind=request_received` so the list is one entry per gateway
/// call (complete audit anchor). Pass `?all=true` to list every event.
pub async fn list_traces(
    State(state): State<Arc<DaemonState>>,
    axum::extract::Query(q): axum::extract::Query<ListTracesQuery>,
) -> impl IntoResponse {
    let filter = conduit_trace::TraceFilter {
        limit: q.limit,
        kind: if q.all {
            None
        } else {
            Some("request_received".into())
        },
        ..Default::default()
    };
    match state.trace_store.query(&filter).await {
        Ok(rows) => (StatusCode::OK, Json(json!({"traces": rows}))).into_response(),
        Err(e) => internal(e).into_response(),
    }
}

/// GET /admin/traces/{id} — complete audit trail for a request.
///
/// Accepts either an event id or a shared `trace_id`. Returns the full event
/// bundle (request body, routing, response body, usage) in chronological order.
pub async fn get_trace(
    State(state): State<Arc<DaemonState>>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    match state.trace_store.get_bundle(&id).await {
        Ok(Some(events)) => {
            let trace_id = events
                .first()
                .map(|e| {
                    if e.trace_id.is_empty() {
                        e.id.clone()
                    } else {
                        e.trace_id.clone()
                    }
                })
                .unwrap_or_else(|| id.clone());
            // Flatten request/response for easy UI consumption while keeping
            // the raw event timeline for complete audit.
            // Prefer original wire bodies; expose stream SSE frames when present.
            let mut wire_format: Option<String> = None;
            let mut request_ir: Option<serde_json::Value> = None;
            let mut request_headers: Option<serde_json::Value> = None;
            let request = events.iter().find_map(|e| match &e.kind {
                conduit_ir::trace::TraceEventKind::RequestReceived {
                    request,
                    request_ir: ir,
                    wire_format: wf,
                    request_headers: rh,
                    ..
                } => {
                    wire_format = wf.clone();
                    request_ir = ir.clone();
                    request_headers = rh.clone();
                    Some(request.clone())
                }
                _ => None,
            });
            let mut stream = false;
            let mut stream_frames: Option<Vec<String>> = None;
            let mut response_headers: Option<serde_json::Value> = None;
            let response = events.iter().rev().find_map(|e| match &e.kind {
                conduit_ir::trace::TraceEventKind::UpstreamResponse {
                    response: r,
                    wire_format: wf,
                    stream: s,
                    stream_frames: frames,
                    response_headers: rh,
                    ..
                } => {
                    stream = *s;
                    stream_frames = frames.clone();
                    response_headers = rh.clone();
                    if wire_format.is_none() {
                        wire_format = wf.clone();
                    }
                    r.clone()
                }
                _ => None,
            });
            (
                StatusCode::OK,
                Json(json!({
                    "trace_id": trace_id,
                    "events": events,
                    "request": request,
                    "request_ir": request_ir,
                    "request_headers": request_headers,
                    "response": response,
                    "response_headers": response_headers,
                    "wire_format": wire_format,
                    "stream": stream,
                    "stream_frames": stream_frames,
                })),
            )
                .into_response()
        }
        Ok(None) => err(StatusCode::NOT_FOUND, "trace not found").into_response(),
        Err(e) => internal(e).into_response(),
    }
}

/// SSE event name for broadcast subscriber lag notifications (KD-13 / PR1b).
pub const SSE_EVENT_LAGGED: &str = "lagged";

/// JSON body for `event: lagged` frames: `{"skipped":N}`.
///
/// Shared with unit tests and CLI contract — do not invent alternate shapes.
pub fn format_lagged_sse_data(skipped: u64) -> String {
    serde_json::json!({ "skipped": skipped }).to_string()
}

/// Build the wire-text of one lagged SSE frame (event + data + blank line).
pub fn format_lagged_sse_frame(skipped: u64) -> String {
    format!(
        "event: {}\ndata: {}\n\n",
        SSE_EVENT_LAGGED,
        format_lagged_sse_data(skipped)
    )
}

/// GET /admin/traces/stream — SSE of live events after durable write.
pub async fn stream_traces(State(state): State<Arc<DaemonState>>) -> impl IntoResponse {
    use axum::response::sse::{Event, KeepAlive, Sse};
    use futures::stream::StreamExt;

    let mut rx = state.trace_broadcast.subscribe();
    let stream = async_stream::stream! {
        loop {
            match rx.recv().await {
                Ok(ev) => {
                    // Shared with unit tests / CLI contract: full TraceEvent JSON.
                    match crate::server::trace_event_sse_payload(&ev) {
                        Ok(data) => yield Ok::<_, std::convert::Infallible>(Event::default().data(data)),
                        Err(_) => continue,
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                    // Notify client of dropped messages; keep the stream open.
                    let data = format_lagged_sse_data(n);
                    yield Ok::<_, std::convert::Infallible>(
                        Event::default().event(SSE_EVENT_LAGGED).data(data),
                    );
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            }
        }
    };

    Sse::new(stream.map(|r| r))
        .keep_alive(KeepAlive::default())
        .into_response()
}

#[derive(Debug, Deserialize)]
pub struct ReplayQuery {
    /// Default true: never call upstream / never bill.
    #[serde(default = "default_true")]
    pub dry_run: bool,
}

fn default_true() -> bool {
    true
}

/// POST /admin/traces/{id}/replay — reconstruct routing plan from a stored event.
///
/// Dry-run (default) returns the intended provider/target and a request summary
/// without invoking upstream or charging quota.
pub async fn replay_trace(
    State(state): State<Arc<DaemonState>>,
    Path(id): Path<String>,
    axum::extract::Query(q): axum::extract::Query<ReplayQuery>,
) -> impl IntoResponse {
    if !q.dry_run {
        return err(
            StatusCode::NOT_IMPLEMENTED,
            "live replay (dry_run=false) is not implemented; use dry_run=true (default)",
        )
        .into_response();
    }

    let event = match state.trace_store.get_full(&id).await {
        Ok(Some(ev)) => ev,
        Ok(None) => return err(StatusCode::NOT_FOUND, "trace not found").into_response(),
        Err(e) => return internal(e).into_response(),
    };

    let table = state.routing_table.load_full();
    let plan = build_replay_plan(&event, &table);
    (StatusCode::OK, Json(json!(plan))).into_response()
}

/// Pure planner used by replay dry-run (and unit tests).
///
/// Never invokes upstream or mutates usage — dry-run only.
pub fn build_replay_plan(
    event: &conduit_ir::trace::TraceEvent,
    table: &conduit_router::table::RoutingTable,
) -> Value {
    use conduit_ir::trace::TraceEventKind;
    use conduit_router::decision::route;

    match &event.kind {
        TraceEventKind::RequestReceived {
            alias,
            stream,
            downstream_key_id,
            request,
            wire_format,
            ..
        } => {
            let decision = route(alias, table, 0).ok();
            json!({
                "dry_run": true,
                "trace_id": event.id,
                "event_kind": "request_received",
                "request_summary": {
                    "alias": alias,
                    "stream": stream,
                    "downstream_key_id": downstream_key_id,
                    "wire_format": wire_format,
                    "request": request,
                },
                "intended_target": decision.as_ref().map(|d| json!({
                    "provider_id": d.provider_id,
                    "provider_kind": d.provider_kind,
                    "model_id": d.model_id,
                    "upstream_key_id": d.upstream_key_id,
                    "base_url": d.base_url,
                    "attempt_no": d.attempt_no,
                })),
                "routing_error": if decision.is_none() {
                    Some(format!("no route for alias '{alias}' in current table"))
                } else {
                    None
                },
                "upstream_called": false,
                "billed": false,
            })
        }
        TraceEventKind::RoutingDecided {
            provider_id,
            model_id,
            upstream_key_id,
            attempt_no,
            ..
        } => {
            json!({
                "dry_run": true,
                "trace_id": event.id,
                "event_kind": "routing_decided",
                "request_summary": {
                    "provider_id": provider_id,
                    "model_id": model_id,
                    "upstream_key_id": upstream_key_id,
                    "attempt_no": attempt_no,
                },
                "intended_target": {
                    "provider_id": provider_id,
                    "model_id": model_id,
                    "upstream_key_id": upstream_key_id,
                    "attempt_no": attempt_no,
                },
                "upstream_called": false,
                "billed": false,
            })
        }
        other => {
            let kind_label = match other {
                TraceEventKind::UpstreamResponse { .. } => "upstream_response",
                TraceEventKind::FinalUsage { .. } => "final_usage",
                TraceEventKind::Error { .. } => "error",
                _ => "other",
            };
            json!({
                "dry_run": true,
                "trace_id": event.id,
                "event_kind": kind_label,
                "request_summary": event.kind,
                "intended_target": null,
                "note": "event kind has no full request body; only summary available",
                "upstream_called": false,
                "billed": false,
            })
        }
    }
}

#[cfg(test)]
mod replay_tests {
    use conduit_ir::trace::{TraceEvent, TraceEventKind};
    use conduit_router::{
        policy::RetryPolicy,
        table::{Route, RouteTarget, RoutingStrategy, RoutingTable},
    };

    use super::*;

    fn sample_table() -> RoutingTable {
        RoutingTable::new(vec![Route {
            alias: "gpt-4o".into(),
            strategy: RoutingStrategy::Fixed,
            targets: vec![RouteTarget {
                provider_id: "openai".into(),
                model_id: "gpt-4o".into(),
                upstream_key_id: "uk1".into(),
                provider_kind: "openai".into(),
                base_url: Some("https://api.openai.com".into()),
            }],
            retry_policy: RetryPolicy::default(),
        }])
    }

    #[test]
    fn dry_run_plan_from_request_received_resolves_target() {
        let table = sample_table();
        let event = TraceEvent::new(TraceEventKind::RequestReceived {
            downstream_key_id: Some("dk".into()),
            alias: "gpt-4o".into(),
            stream: false,
            request: serde_json::json!({"alias":"gpt-4o"}),
            request_ir: None,
            wire_format: None,
            request_headers: None,
        });

        let plan = build_replay_plan(&event, &table);
        assert_eq!(plan["dry_run"], true);
        assert_eq!(plan["billed"], false);
        assert_eq!(plan["upstream_called"], false);
        assert_eq!(plan["event_kind"], "request_received");
        assert_eq!(plan["request_summary"]["alias"], "gpt-4o");
        assert_eq!(plan["intended_target"]["provider_id"], "openai");
        assert_eq!(plan["intended_target"]["provider_kind"], "openai");
        assert_eq!(plan["intended_target"]["model_id"], "gpt-4o");
        assert!(plan["routing_error"].is_null());
    }

    #[test]
    fn dry_run_unknown_alias_reports_routing_error_without_billing() {
        let table = sample_table();
        let event = TraceEvent::new(TraceEventKind::RequestReceived {
            downstream_key_id: None,
            alias: "missing-model".into(),
            stream: true,
            request: serde_json::json!({}),
            request_ir: None,
            wire_format: None,
            request_headers: None,
        });
        let plan = build_replay_plan(&event, &table);
        assert_eq!(plan["dry_run"], true);
        assert_eq!(plan["upstream_called"], false);
        assert_eq!(plan["billed"], false);
        assert!(plan["intended_target"].is_null());
        assert!(plan["routing_error"]
            .as_str()
            .unwrap()
            .contains("missing-model"));
    }
}

#[cfg(test)]
mod lagged_sse_tests {
    use super::{format_lagged_sse_data, format_lagged_sse_frame, SSE_EVENT_LAGGED};

    #[test]
    fn lagged_data_json_has_skipped_count() {
        let data = format_lagged_sse_data(42);
        let v: serde_json::Value = serde_json::from_str(&data).expect("json");
        assert_eq!(v["skipped"], 42);
        // Only the contracted field — no inventing alternate shapes.
        assert!(v.as_object().unwrap().contains_key("skipped"));
    }

    #[test]
    fn lagged_frame_is_event_lagged_with_json_data() {
        let frame = format_lagged_sse_frame(7);
        assert!(
            frame.starts_with(&format!("event: {SSE_EVENT_LAGGED}\n")),
            "frame must start with event: lagged, got {frame:?}"
        );
        assert!(frame.contains("data: {\"skipped\":7}"));
        // Blank-line delimiter ends the frame (SSE wire contract).
        assert!(frame.ends_with("\n\n"));
        // Normal trace frames use only data:; lagged must be discriminable.
        assert!(!frame.contains("request_received"));
    }

    #[test]
    fn lagged_zero_is_valid() {
        let data = format_lagged_sse_data(0);
        let v: serde_json::Value = serde_json::from_str(&data).unwrap();
        assert_eq!(v["skipped"], 0);
    }
}
