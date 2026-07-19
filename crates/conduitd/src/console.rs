//! Console API handlers — CRUD for providers, routes, downstream keys, usage, pricing.

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
    /// If omitted, the secret must be set separately via PUT /console/providers/{id}/secret.
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
    let upstream_key_ref = format!("secret://upstream_key/{}", id);

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

/// PUT /console/providers/{id}/secret — store or rotate the upstream API key.
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

    // Generate a cryptographically random key: "sk_" + 32-byte random hex
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
    let raw_key = format!("sk_{}", hex::encode(rand_bytes));
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

    if let Err(e) = repo.update(&updated).await {
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
// Usage ledger (per-request consumption)
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

/// GET /console/usage — recent per-request consumption rows.
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
    /// Optional downstream key scope for `by_day` / `by_model` rollups.
    pub key_id: Option<String>,
}

/// GET /console/usage/summary — aggregate spend by key / day / model for a calendar month.
pub async fn usage_summary(
    State(state): State<Arc<DaemonState>>,
    Query(q): Query<UsageSummaryQuery>,
) -> impl IntoResponse {
    use chrono::Datelike;
    let now = Utc::now();
    let period = q
        .period
        .unwrap_or_else(|| format!("{:04}-{:02}", now.year(), now.month()));
    let key_id = q.key_id.as_deref().filter(|s| !s.is_empty());
    let repo = UsageRepo::new(&state.pool);

    let entries = match repo.summary_period(&period).await {
        Ok(e) => e,
        Err(e) => return internal(e).into_response(),
    };
    let by_day = match repo.summary_by_day(&period, key_id).await {
        Ok(d) => d,
        Err(e) => return internal(e).into_response(),
    };
    let by_model = match repo.summary_by_model(&period, key_id).await {
        Ok(m) => m,
        Err(e) => return internal(e).into_response(),
    };

    // Top-level totals: when a key filter is set, sum only that key's entry so
    // cards match the scoped day/model charts.
    let scoped_entries: Vec<_> = match key_id {
        Some(kid) => entries
            .iter()
            .filter(|e| e.downstream_key_id == kid)
            .collect(),
        None => entries.iter().collect(),
    };
    let total_usd: f64 = scoped_entries.iter().map(|e| e.total_usd).sum();
    let request_count: u64 = scoped_entries.iter().map(|e| e.request_count).sum();

    (
        StatusCode::OK,
        Json(json!({
            "period": period,
            "total_usd": total_usd,
            "request_count": request_count,
            "key_id": key_id,
            "entries": entries.iter().map(|e| json!({
                "downstream_key_id": e.downstream_key_id,
                "request_count": e.request_count,
                "total_usd": e.total_usd,
                "prompt_tokens": e.prompt_tokens,
                "completion_tokens": e.completion_tokens,
                "total_tokens": e.total_tokens,
            })).collect::<Vec<_>>(),
            "by_day": by_day.iter().map(|d| json!({
                "day": d.day,
                "request_count": d.request_count,
                "total_usd": d.total_usd,
                "total_tokens": d.total_tokens,
            })).collect::<Vec<_>>(),
            "by_model": by_model.iter().map(|m| json!({
                "label": m.label,
                "provider_kind": m.provider_kind,
                "request_count": m.request_count,
                "total_usd": m.total_usd,
                "total_tokens": m.total_tokens,
            })).collect::<Vec<_>>(),
        })),
    )
        .into_response()
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

/// Optional body for `POST /console/pricing/sync`.
#[derive(Debug, Default, Deserialize)]
pub struct SyncPricingBody {
    /// Override LiteLLM cost-map URL (default: GitHub raw main).
    #[serde(default)]
    pub url: Option<String>,
}

/// POST /console/pricing/sync — fetch LiteLLM price map, convert, cache, reload.
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
