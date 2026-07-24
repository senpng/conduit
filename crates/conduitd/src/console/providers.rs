//! Console providers + secrets CRUD.

use std::sync::Arc;

use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
    Json,
};
use chrono::Utc;
use conduit_store::{schema::ProviderRow, ProviderRepo};
use serde::Deserialize;
use serde_json::json;
use ulid::Ulid;

use super::common::{err, internal};
use crate::{server::reload_routing_table, state::DaemonState};

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

