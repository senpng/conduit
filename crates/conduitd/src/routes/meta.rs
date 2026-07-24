//! Lightweight gateway meta endpoints.

use std::sync::Arc;

use axum::{extract::State, response::IntoResponse, Json};
use serde_json::json;

use crate::state::DaemonState;

/// GET /v1/models — OpenAI-compatible list from the live routing table.
///
/// When model limits are known for a route target, includes `context_window`
/// and `context_length` (from LiteLLM `max_input_tokens`). Omits those fields
/// when no limit is known — does not invent a default window.
pub async fn list_models(State(state): State<Arc<DaemonState>>) -> impl IntoResponse {
    let table = state.routing_table.load();
    let limits = state.limits_table.load();
    let routes = table.iter().map(|route| {
        let target = route.targets.first();
        let owned_by = target
            .map(|t| t.provider_kind.clone())
            .unwrap_or_else(|| "conduit".into());
        let model_id = target
            .map(|t| t.model_id.clone())
            .unwrap_or_else(|| route.alias.clone());
        (route.alias.clone(), owned_by, model_id)
    });
    let data = crate::state::build_models_list_data(routes, &limits);
    Json(json!({
        "object": "list",
        "data": data,
    }))
}

/// GET /health
pub async fn health(State(state): State<Arc<DaemonState>>) -> impl IntoResponse {
    Json(json!({
        "status": "ok",
        "version": state.version,
    }))
}
