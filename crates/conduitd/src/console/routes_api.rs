//! Console routes CRUD.

use std::sync::Arc;

use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
    Json,
};
use chrono::Utc;
use conduit_store::{schema::RouteRow, RouteRepo};
use serde::Deserialize;
use serde_json::{json, Value};
use ulid::Ulid;

use super::common::{err, internal};
use crate::{server::reload_routing_table, state::DaemonState};


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

