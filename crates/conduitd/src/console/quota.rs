//! Console cooldowns and OAuth quota snapshots.

use std::sync::Arc;

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::IntoResponse,
    Json,
};
use conduit_store::ProviderRepo;
use serde::Deserialize;
use serde_json::json;

use super::common::{err, internal};
use crate::state::DaemonState;

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
