//! Console pricing table, overrides, reload, and sync.

use std::sync::Arc;

use axum::{
    extract::{Query, State},
    http::StatusCode,
    response::IntoResponse,
    Json,
};
use chrono::Utc;
use serde::Deserialize;
use serde_json::json;

use super::common::{err, internal};
use crate::state::DaemonState;

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

