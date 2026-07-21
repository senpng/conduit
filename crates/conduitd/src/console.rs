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

/// Reject nonsensical rate limits. The value is stored as `i64` but later cast
/// to `u32` for enforcement, so a negative wraps to a near-infinite limit
/// (rate-limiting silently disabled) and `0` rejects every request (the key is
/// bricked). Only a positive requests/minute is meaningful.
fn validate_rpm(rpm: Option<i64>) -> Result<(), &'static str> {
    match rpm {
        Some(v) if v < 1 => Err("rate_limit_rpm must be a positive number"),
        _ => Ok(()),
    }
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
        deleted_at: None,
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
            // Roll back incomplete create (hard delete — never partially exposed).
            let _ = repo.hard_delete(&id).await;
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
    // Soft-delete: keep the row and secret for audit / potential restore.
    match repo.delete(&id).await {
        Ok(()) => {
            if let Err(e) = reload_routing_table(&state).await {
                tracing::warn!(
                    "provider soft-deleted but routing table reload failed: {}",
                    e
                );
            }
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

/// GET /console/providers/{id}/secret — decrypt and return the upstream secret.
///
/// Loopback console only. Returns either a plaintext API key or a full OAuth
/// credential bundle (tokens + account metadata).
pub async fn get_provider_secret(
    State(state): State<Arc<DaemonState>>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    use secrecy::ExposeSecret;

    let repo = ProviderRepo::new(&state.pool);
    let row = match repo.get(&id).await {
        Ok(Some(r)) => r,
        Ok(None) => return err(StatusCode::NOT_FOUND, "provider not found").into_response(),
        Err(e) => return internal(e).into_response(),
    };

    let key_id = conduit_store::secret_key_id_from_ref(&row.upstream_key_ref, &id);
    let raw = match state.secret_backend.get("upstream_key", &key_id).await {
        Ok(Some(s)) => s,
        Ok(None) => {
            return err(
                StatusCode::NOT_FOUND,
                format!("no secret stored for provider '{id}' (key_id={key_id})"),
            )
            .into_response();
        }
        Err(e) => {
            return err(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("failed to decrypt secret: {e}"),
            )
            .into_response();
        }
    };

    let bytes = raw.expose_secret();
    let plaintext = String::from_utf8_lossy(bytes).to_string();

    if let Some(cred) = conduit_oauth::OAuthCredential::try_parse_secret(bytes) {
        let expired_local = cred.expired.clone();
        return (
            StatusCode::OK,
            Json(json!({
                "provider_id": id,
                "provider_name": row.name,
                "provider_kind": row.kind,
                "secret_kind": "oauth",
                "key_id": key_id,
                "oauth": {
                    "type": cred.provider_type,
                    "auth_kind": cred.auth_kind,
                    "access_token": cred.access_token,
                    "refresh_token": cred.refresh_token,
                    "id_token": cred.id_token,
                    "token_type": cred.token_type,
                    "expired": expired_local,
                    "last_refresh": cred.last_refresh,
                    "email": cred.email,
                    "account_id": cred.account_id,
                    "plan_type": cred.plan_type,
                    "organization_id": cred.organization_id,
                    "organization_name": cred.organization_name,
                    "sub": cred.sub,
                    "base_url": cred.base_url,
                    "token_endpoint": cred.token_endpoint,
                    "proxy_url": cred.proxy_url,
                    "using_api": cred.using_api,
                    "extra": cred.extra,
                },
            })),
        )
            .into_response();
    }

    (
        StatusCode::OK,
        Json(json!({
            "provider_id": id,
            "provider_name": row.name,
            "provider_kind": row.kind,
            "secret_kind": "api_key",
            "key_id": key_id,
            "api_key": plaintext,
        })),
    )
        .into_response()
}

// ═══════════════════════════════════════════════════════════════════════════════
// Routes
// ═══════════════════════════════════════════════════════════════════════════════

#[derive(Debug, Deserialize)]
pub struct CreateRouteBody {
    pub match_alias: String,
    /// "fixed", "fallback", or "weighted"
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
        deleted_at: None,
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
    if let Err(e) = validate_rpm(body.rate_limit_rpm) {
        return err(StatusCode::BAD_REQUEST, e).into_response();
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
        deleted_at: None,
    };

    let repo = KeyRepo::new(&state.pool);
    match repo.insert(&row).await {
        Ok(()) => {
            // Store the raw token encrypted so it can be revealed later
            // (mirrors provider upstream keys). Only the hash lives in SQLite.
            let secret = secrecy::SecretVec::new(raw_key.clone().into_bytes());
            if let Err(e) = state.secret_backend.put("downstream_key", &id, secret).await {
                // Roll back so we never keep a key whose token can't be revealed.
                let _ = repo.delete(&id).await;
                return err(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("failed to store key secret: {e}"),
                )
                .into_response();
            }
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

/// GET /console/keys/{id}/secret — decrypt and return the raw downstream token.
///
/// Mirrors [`get_provider_secret`]. Only keys created with reveal support have a
/// stored token; older keys return 404.
pub async fn get_key_secret(
    State(state): State<Arc<DaemonState>>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    use secrecy::ExposeSecret;

    let repo = KeyRepo::new(&state.pool);
    let row = match repo.get(&id).await {
        Ok(Some(r)) => r,
        Ok(None) => return err(StatusCode::NOT_FOUND, "key not found").into_response(),
        Err(e) => return internal(e).into_response(),
    };

    match state.secret_backend.get("downstream_key", &id).await {
        Ok(Some(s)) => {
            let key = String::from_utf8_lossy(s.expose_secret()).to_string();
            (
                StatusCode::OK,
                Json(json!({
                    "id": id,
                    "name": row.name,
                    "secret_kind": "api_key",
                    "key": key,
                })),
            )
                .into_response()
        }
        Ok(None) => err(
            StatusCode::NOT_FOUND,
            format!("no stored token for key '{id}'"),
        )
        .into_response(),
        Err(e) => err(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("failed to decrypt key secret: {e}"),
        )
        .into_response(),
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
    if let Err(e) = validate_rpm(body.rate_limit_rpm) {
        return err(StatusCode::BAD_REQUEST, e).into_response();
    }

    let now = Utc::now().to_rfc3339();
    // Full form updates (TUI edit) send name + model_whitelist together; in that
    // case rate_limit_rpm is authoritative (None = unlimited). Partial updates
    // that only touch rpm still use Some → set / None → keep.
    let full_form = body.name.is_some() && body.model_whitelist.is_some();
    let rate_limit_rpm = if full_form {
        body.rate_limit_rpm
    } else if body.rate_limit_rpm.is_some() {
        body.rate_limit_rpm
    } else {
        existing.rate_limit_rpm
    };
    let updated = DownstreamKeyRow {
        name: body.name.unwrap_or(existing.name),
        model_whitelist: body
            .model_whitelist
            .map(|v| serde_json::to_string(&v).unwrap_or_else(|_| "[]".to_string()))
            .unwrap_or(existing.model_whitelist),
        monthly_budget_usd: None,
        rate_limit_rpm,
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
    /// Row offset for pagination (0-based).
    #[serde(default)]
    pub offset: usize,
    pub key_id: Option<String>,
    /// Optional calendar month (`YYYY-MM`) in the client timezone.
    pub period: Option<String>,
    /// Free-text filter (model / alias / provider / request id / key id).
    pub q: Option<String>,
    /// Sort: `date` (default) | `cost` | `tokens` — always descending.
    pub sort: Option<String>,
    /// Minutes east of UTC for calendar day/month bucketing (e.g. 480 for CST).
    pub tz_offset_minutes: Option<i32>,
}

fn default_usage_limit() -> usize {
    50
}

// ═══════════════════════════════════════════════════════════════════════════════
// Upstream provider cooldowns (429 / usage_limit)
// ═══════════════════════════════════════════════════════════════════════════════

/// GET /console/cooldowns — list providers currently skipped due to upstream quota/rate limit.
pub async fn list_cooldowns(State(state): State<Arc<DaemonState>>) -> impl IntoResponse {
    let entries = state.cooldown.list();
    (StatusCode::OK, Json(json!({ "entries": entries }))).into_response()
}

/// DELETE /console/cooldowns/{provider_id} — clear one provider cooldown (CLIProxyAPI reset-quota).
pub async fn clear_provider_cooldown(
    State(state): State<Arc<DaemonState>>,
    Path(provider_id): Path<String>,
) -> impl IntoResponse {
    let cleared = state.cooldown.clear(&provider_id);
    (
        StatusCode::OK,
        Json(json!({ "provider_id": provider_id, "cleared": cleared })),
    )
        .into_response()
}

/// DELETE /console/cooldowns — clear all cooldowns.
pub async fn clear_all_cooldowns(State(state): State<Arc<DaemonState>>) -> impl IntoResponse {
    state.cooldown.clear_all();
    (StatusCode::OK, Json(json!({ "cleared": true }))).into_response()
}

// ═══════════════════════════════════════════════════════════════════════════════
// Upstream quota snapshots (headers / 429 body — best-effort remaining)
// ═══════════════════════════════════════════════════════════════════════════════

/// GET /console/quota-snapshots — last-seen rate-limit signals for all providers.
pub async fn list_quota_snapshots(State(state): State<Arc<DaemonState>>) -> impl IntoResponse {
    let entries = state.quota_snapshots.list();
    (StatusCode::OK, Json(json!({ "entries": entries }))).into_response()
}

/// GET /console/quota-snapshots/{provider_id}
pub async fn get_quota_snapshot(
    State(state): State<Arc<DaemonState>>,
    Path(provider_id): Path<String>,
) -> impl IntoResponse {
    match state.quota_snapshots.get(&provider_id) {
        Some(s) => (StatusCode::OK, Json(json!(s))).into_response(),
        None => err(
            StatusCode::NOT_FOUND,
            format!("no quota snapshot for provider '{provider_id}' (call upstream first)"),
        )
        .into_response(),
    }
}

/// POST /console/quota-snapshots/{provider_id}/refresh
///
/// For OAuth providers (`claude-oauth` / `codex-oauth` / `grok-oauth`), probes the
/// upstream usage/billing API and updates the snapshot. For others, re-reads
/// last-seen headers. Optional `?clear_cooldown=true` clears the provider cooldown.
pub async fn refresh_quota_snapshot(
    State(state): State<Arc<DaemonState>>,
    Path(provider_id): Path<String>,
    Query(q): Query<RefreshQuotaQuery>,
) -> impl IntoResponse {
    if q.clear_cooldown.unwrap_or(false) {
        let _ = state.cooldown.clear(&provider_id);
    }
    let probe = probe_oauth_quota(&state, &provider_id).await;
    let snap = state.quota_snapshots.get(&provider_id);
    let cooling = state.cooldown.remaining(&provider_id).map(|d| d.as_secs());
    match probe {
        Ok(Some(note)) => (
            StatusCode::OK,
            Json(json!({
                "provider_id": provider_id,
                "snapshot": snap,
                "cooldown_remaining_secs": cooling,
                "probed": true,
                "note": note,
            })),
        )
            .into_response(),
        Ok(None) => (
            StatusCode::OK,
            Json(json!({
                "provider_id": provider_id,
                "snapshot": snap,
                "cooldown_remaining_secs": cooling,
                "probed": false,
                "note": "No OAuth usage probe for this provider; snapshot is last-seen headers/error body.",
            })),
        )
            .into_response(),
        Err(e) => {
            // Still return last snapshot so the UI can show stale data + error.
            (
                StatusCode::OK,
                Json(json!({
                    "provider_id": provider_id,
                    "snapshot": snap,
                    "cooldown_remaining_secs": cooling,
                    "probed": false,
                    "error": e,
                })),
            )
                .into_response()
        }
    }
}

/// POST /console/quota-snapshots/refresh — probe all OAuth providers.
pub async fn refresh_all_quota_snapshots(
    State(state): State<Arc<DaemonState>>,
) -> impl IntoResponse {
    let repo = ProviderRepo::new(&state.pool);
    let providers = match repo.list().await {
        Ok(p) => p,
        Err(e) => return internal(e).into_response(),
    };
    let mut results = Vec::new();
    let oauth: Vec<_> = providers
        .into_iter()
        .filter(|p| is_oauth_provider_kind(&p.kind))
        .collect();
    // Probe all OAuth providers concurrently — serial awaits made the TUI wait
    // for every account's usage round-trip before the first refresh returned.
    let state_ref = &state;
    let probed = futures::future::join_all(
        oauth
            .iter()
            .map(|p| async move { (p, probe_oauth_quota(state_ref, &p.id).await) }),
    )
    .await;
    for (p, probe) in probed {
        match probe {
            Ok(Some(note)) => results.push(json!({
                "provider_id": p.id,
                "kind": p.kind,
                "ok": true,
                "note": note,
                "snapshot": state.quota_snapshots.get(&p.id),
            })),
            Ok(None) => results.push(json!({
                "provider_id": p.id,
                "kind": p.kind,
                "ok": true,
                "note": "skipped",
            })),
            Err(e) => results.push(json!({
                "provider_id": p.id,
                "kind": p.kind,
                "ok": false,
                "error": e,
                "snapshot": state.quota_snapshots.get(&p.id),
            })),
        }
    }
    (
        StatusCode::OK,
        Json(json!({
            "entries": state.quota_snapshots.list(),
            "results": results,
        })),
    )
        .into_response()
}

#[derive(Debug, Deserialize)]
pub struct RefreshQuotaQuery {
    pub clear_cooldown: Option<bool>,
}

fn is_oauth_provider_kind(kind: &str) -> bool {
    matches!(
        kind.trim().to_ascii_lowercase().as_str(),
        "claude-oauth" | "codex-oauth" | "grok-oauth" | "anthropic-oauth"
    )
}

/// Probe Claude / Codex / Grok OAuth remaining via usage (or billing) API; store snapshot.
///
/// Returns `Ok(Some(note))` when probed, `Ok(None)` when not an OAuth kind that
/// supports probing, `Err` on probe failure.
async fn probe_oauth_quota(
    state: &DaemonState,
    provider_id: &str,
) -> Result<Option<String>, String> {
    let repo = ProviderRepo::new(&state.pool);
    let row = repo
        .get(provider_id)
        .await
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("provider '{provider_id}' not found"))?;

    let kind = match conduit_oauth::OAuthProviderKind::parse(&row.kind) {
        Ok(k) => k,
        Err(_) => return Ok(None),
    };

    let key_id = conduit_store::secret_key_id_from_ref(&row.upstream_key_ref, provider_id);

    let store = Arc::new(crate::oauth::BackendSecretStore::new(
        state.secret_backend.clone(),
    ));
    let resolver =
        conduit_oauth::CredentialResolver::new(store).with_default_proxy(state.proxy_url.clone());
    let resolved = resolver
        .resolve(&key_id)
        .await
        .map_err(|e| e.to_string())?;

    let proxy = conduit_oauth::resolve_effective_proxy(None, state.proxy_url.as_deref());

    let usage = conduit_oauth::fetch_oauth_usage(kind, &resolved, proxy.as_deref())
        .await
        .map_err(|e| e.to_string())?;

    let mut details = std::collections::HashMap::new();
    details.insert("raw_excerpt".into(), usage.raw_excerpt.clone());
    if let Some(l) = &resolved.label {
        details.insert("account".into(), l.clone());
    }
    details.insert("kind".into(), kind.as_str().into());

    let session_pct = usage.session.as_ref().map(|w| w.remaining_pct);
    let session_reset = usage.session.as_ref().and_then(|w| w.resets_at.clone());
    let weekly_pct = usage.weekly.as_ref().map(|w| w.remaining_pct);
    let weekly_reset = usage.weekly.as_ref().and_then(|w| w.resets_at.clone());

    state.quota_snapshots.record_oauth_usage(
        provider_id,
        usage.source,
        session_pct,
        session_reset,
        weekly_pct,
        weekly_reset,
        details,
    );

    let label = conduit_oauth::format_remaining_short(&usage);
    Ok(Some(format!("oauth remaining: {label}")))
}

/// GET /console/usage — paginated per-request consumption rows.
///
/// Query: `limit`, `offset`, `key_id`, `period` (`YYYY-MM`), `q` (filter),
/// `sort` (`date`|`cost`|`tokens`), `tz_offset_minutes`. Response includes
/// `total` for pagination. Period uses the client local calendar.
pub async fn list_usage(
    State(state): State<Arc<DaemonState>>,
    Query(q): Query<ListUsageQuery>,
) -> impl IntoResponse {
    use conduit_store::{clamp_tz_offset_minutes, UsageListOpts, UsageListSort};
    let repo = UsageRepo::new(&state.pool);
    let sort = q
        .sort
        .as_deref()
        .map(UsageListSort::parse)
        .unwrap_or_default();
    let tz = clamp_tz_offset_minutes(q.tz_offset_minutes.unwrap_or(0));
    match repo
        .list_page(UsageListOpts {
            limit: q.limit,
            offset: q.offset,
            key_id: q.key_id.as_deref(),
            period: q.period.as_deref(),
            q: q.q.as_deref(),
            sort,
            tz_offset_minutes: tz,
        })
        .await
    {
        Ok(page) => (
            StatusCode::OK,
            Json(json!({
                "entries": page.rows,
                "total": page.total,
                "limit": page.limit,
                "offset": page.offset,
                "sort": sort.as_str(),
            })),
        )
            .into_response(),
        Err(e) => internal(e).into_response(),
    }
}

#[derive(Debug, Deserialize)]
pub struct UsageSummaryQuery {
    /// `YYYY-MM` calendar month, or `all` for lifetime totals.
    /// Defaults to the current month in the client timezone.
    pub period: Option<String>,
    /// Optional downstream key scope for `by_day` / `by_model` rollups.
    pub key_id: Option<String>,
    /// Minutes east of UTC for calendar day/month bucketing (e.g. 480 for CST).
    pub tz_offset_minutes: Option<i32>,
}

/// GET /console/usage/summary — aggregate spend by key / day / model.
///
/// `period=YYYY-MM` scopes to a **local** calendar month (see `tz_offset_minutes`);
/// `period=all` is lifetime.
pub async fn usage_summary(
    State(state): State<Arc<DaemonState>>,
    Query(q): Query<UsageSummaryQuery>,
) -> impl IntoResponse {
    use chrono::{Datelike, FixedOffset, Utc};
    use conduit_store::clamp_tz_offset_minutes;

    let tz = clamp_tz_offset_minutes(q.tz_offset_minutes.unwrap_or(0));
    let offset = FixedOffset::east_opt(tz * 60).unwrap_or(FixedOffset::east_opt(0).unwrap());
    let local_now = Utc::now().with_timezone(&offset);
    let period = match q.period.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        Some(p) if p.eq_ignore_ascii_case("all") => "all".to_string(),
        Some(p) => p.to_string(),
        None => format!("{:04}-{:02}", local_now.year(), local_now.month()),
    };
    let key_id = q.key_id.as_deref().filter(|s| !s.is_empty());
    let repo = UsageRepo::new(&state.pool);

    let entries = match repo.summary_period(&period, tz).await {
        Ok(e) => e,
        Err(e) => return internal(e).into_response(),
    };
    let by_day = match repo.summary_by_day(&period, key_id, tz).await {
        Ok(d) => d,
        Err(e) => return internal(e).into_response(),
    };
    // Trailing ~400 local days for the GitHub-style contribution graph (52 weeks).
    let since = (local_now.date_naive() - chrono::Duration::days(400))
        .format("%Y-%m-%d")
        .to_string();
    let by_day_trailing = match repo.summary_by_day_since(&since, key_id, tz).await {
        Ok(d) => d,
        Err(e) => return internal(e).into_response(),
    };
    let by_model = match repo.summary_by_model(&period, key_id, tz).await {
        Ok(m) => m,
        Err(e) => return internal(e).into_response(),
    };
    // Nested model breakdowns for Usage UI detail panes (by key / by day).
    let by_key_model = match repo.summary_by_key_model(&period, tz).await {
        Ok(m) => m,
        Err(e) => return internal(e).into_response(),
    };
    let by_day_model = match repo.summary_by_day_model(&period, tz).await {
        Ok(m) => m,
        Err(e) => return internal(e).into_response(),
    };
    let outcome = match repo.summary_outcome(&period, key_id, tz).await {
        Ok(o) => o,
        Err(e) => return internal(e).into_response(),
    };
    let by_provider = match repo.summary_by_provider(&period, key_id, tz).await {
        Ok(p) => p,
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
    let total_tokens: u64 = scoped_entries.iter().map(|e| e.total_tokens).sum();
    // Prefer outcome request_count (includes zero-token / error rows) when present.
    let request_count: u64 = if outcome.request_count > 0 {
        outcome.request_count
    } else {
        scoped_entries.iter().map(|e| e.request_count).sum()
    };

    (
        StatusCode::OK,
        Json(json!({
            "period": period,
            "total_usd": total_usd,
            "total_tokens": total_tokens,
            "request_count": request_count,
            "success_rate": outcome.success_rate,
            "avg_ttfb_ms": outcome.avg_ttfb_ms,
            "avg_duration_ms": outcome.avg_duration_ms,
            "tokens_per_sec": outcome.tokens_per_sec,
            // Process-lifetime count of ledger records that could not be
            // persisted at all (see usage_wire). For a billing ledger this
            // MUST stay 0; a non-zero value signals a reconciliation gap.
            "ledger_dropped_records": crate::usage_wire::dropped_records(),
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
            "by_day_trailing": by_day_trailing.iter().map(|d| json!({
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
                "tokens_per_sec": m.tokens_per_sec,
            })).collect::<Vec<_>>(),
            "by_key_model": by_key_model.iter().map(|m| json!({
                "downstream_key_id": m.downstream_key_id,
                "label": m.label,
                "provider_kind": m.provider_kind,
                "request_count": m.request_count,
                "total_usd": m.total_usd,
                "total_tokens": m.total_tokens,
            })).collect::<Vec<_>>(),
            "by_day_model": by_day_model.iter().map(|m| json!({
                "day": m.day,
                "label": m.label,
                "provider_kind": m.provider_kind,
                "request_count": m.request_count,
                "total_usd": m.total_usd,
                "total_tokens": m.total_tokens,
            })).collect::<Vec<_>>(),
            "by_provider": by_provider.iter().map(|p| json!({
                "provider_id": p.provider_id,
                "provider_kind": p.provider_kind,
                "request_count": p.request_count,
                "success_count": p.success_count,
                "success_rate": p.success_rate,
                "avg_ttfb_ms": p.avg_ttfb_ms,
                "avg_duration_ms": p.avg_duration_ms,
                "tokens_per_sec": p.tokens_per_sec,
                "total_usd": p.total_usd,
                "total_tokens": p.total_tokens,
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

/// GET /console/pricing/overrides — operator `pricing.json` only (not merged layers).
pub async fn list_pricing_overrides(State(state): State<Arc<DaemonState>>) -> impl IntoResponse {
    match conduit_store::PricingRepo::list_overrides(&state.data_dir).await {
        Ok(rows) => (StatusCode::OK, Json(json!(rows))).into_response(),
        Err(e) => internal(e).into_response(),
    }
}

/// Body for upserting an operator pricing override (USD per million tokens).
#[derive(Debug, Deserialize)]
pub struct UpsertPricingOverrideBody {
    pub provider_kind: String,
    pub model_id: String,
    pub input_per_mtok: f64,
    pub output_per_mtok: f64,
    pub cache_read_per_mtok: Option<f64>,
    pub cache_write_per_mtok: Option<f64>,
    pub reasoning_per_mtok: Option<f64>,
    /// Optional ISO date; defaults to today UTC.
    pub effective_from: Option<String>,
}

/// PUT /console/pricing/overrides — upsert one row into `pricing.json` and reload.
pub async fn upsert_pricing_override(
    State(state): State<Arc<DaemonState>>,
    Json(body): Json<UpsertPricingOverrideBody>,
) -> impl IntoResponse {
    let effective_from = body
        .effective_from
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| Utc::now().format("%Y-%m-%d").to_string());
    let row = conduit_store::schema::PricingRow {
        provider_kind: body.provider_kind.trim().to_string(),
        model_id: body.model_id.trim().to_string(),
        input_per_mtok: body.input_per_mtok,
        output_per_mtok: body.output_per_mtok,
        cache_read_per_mtok: body.cache_read_per_mtok,
        cache_write_per_mtok: body.cache_write_per_mtok,
        reasoning_per_mtok: body.reasoning_per_mtok,
        effective_from,
    };
    match state
        .pricing_repo
        .upsert_override(&state.data_dir, row)
        .await
    {
        Ok(rows) => {
            let map = crate::state::pricing_map_from_repo(&state.pricing_repo).await;
            state.pricing_table.store(std::sync::Arc::new(map));
            (
                StatusCode::OK,
                Json(json!({
                    "status": "upserted",
                    "overrides": rows,
                    "count": rows.len(),
                })),
            )
                .into_response()
        }
        Err(conduit_store::StoreError::Serialization(msg)) => {
            err(StatusCode::BAD_REQUEST, msg).into_response()
        }
        Err(e) => internal(e).into_response(),
    }
}

/// Query for `DELETE /console/pricing/overrides?provider_kind=…&model_id=…`.
///
/// Model ids often contain `/` (e.g. LiteLLM-style paths), so path segments are
/// unreliable; query params avoid 404s from extra path components.
#[derive(Debug, Deserialize)]
pub struct DeletePricingOverrideQuery {
    pub provider_kind: String,
    pub model_id: String,
}

/// DELETE /console/pricing/overrides?provider_kind=…&model_id=…
pub async fn delete_pricing_override(
    State(state): State<Arc<DaemonState>>,
    Query(q): Query<DeletePricingOverrideQuery>,
) -> impl IntoResponse {
    let provider_kind = q.provider_kind.trim();
    let model_id = q.model_id.trim();
    if provider_kind.is_empty() || model_id.is_empty() {
        return err(
            StatusCode::BAD_REQUEST,
            "provider_kind and model_id query params are required",
        )
            .into_response();
    }
    match state
        .pricing_repo
        .delete_override(&state.data_dir, provider_kind, model_id)
        .await
    {
        Ok(rows) => {
            let map = crate::state::pricing_map_from_repo(&state.pricing_repo).await;
            state.pricing_table.store(std::sync::Arc::new(map));
            (
                StatusCode::OK,
                Json(json!({
                    "status": "deleted",
                    "overrides": rows,
                    "count": rows.len(),
                })),
            )
                .into_response()
        }
        Err(conduit_store::StoreError::NotFound(msg)) => {
            err(StatusCode::NOT_FOUND, msg).into_response()
        }
        Err(e) => internal(e).into_response(),
    }
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

            // Same LiteLLM blob also refreshes the separate model-limits table.
            let (limits_rows, limits_source, limits_skipped) = match state
                .limits_repo
                .apply_litellm_json(&state.data_dir, &text)
                .await
            {
                Ok(v) => {
                    let lim_map = crate::state::limits_map_from_repo(&state.limits_repo).await;
                    state.limits_table.store(std::sync::Arc::new(lim_map));
                    v
                }
                Err(e) => {
                    tracing::warn!(error = %e, "LiteLLM limits sync failed (pricing ok)");
                    (0, 0, 0)
                }
            };

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
                    "limits_source_models": limits_source,
                    "limits_skipped": limits_skipped,
                    "limits_total_rows": limits_rows,
                })),
            )
                .into_response()
        }
        Err(e) => internal(e).into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::validate_rpm;

    #[test]
    fn rpm_validation_rejects_zero_and_negative() {
        assert!(validate_rpm(Some(-1)).is_err());
        assert!(validate_rpm(Some(0)).is_err());
        assert!(validate_rpm(Some(1)).is_ok());
        assert!(validate_rpm(Some(600)).is_ok());
        // Absent limit is fine — no rate limiting.
        assert!(validate_rpm(None).is_ok());
    }
}
