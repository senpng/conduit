//! Console downstream keys CRUD.

use std::sync::Arc;

use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
    Json,
};
use chrono::Utc;
use conduit_store::{schema::DownstreamKeyRow, KeyRepo};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use ulid::Ulid;

use super::common::{err, generate_downstream_raw_key, internal, validate_rpm};
use crate::state::DaemonState;


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

    // CSPRNG: sk_ + 32 OS-random bytes as hex (see generate_downstream_raw_key).
    let raw_key = generate_downstream_raw_key();
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

