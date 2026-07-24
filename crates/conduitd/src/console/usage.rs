//! Console usage list + summary.

use std::sync::Arc;

use axum::{
    extract::{Query, State},
    http::StatusCode,
    response::IntoResponse,
    Json,
};
use conduit_store::{KeyRepo, ProviderRepo, UsageRepo};
use serde::Deserialize;
use serde_json::json;

use super::common::internal;
use crate::state::DaemonState;

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

    // Resolve human names for usage labels, including soft-deleted rows so
    // historical rollups keep readable names after operator delete.
    let key_repo = KeyRepo::new(&state.pool);
    let mut key_meta: std::collections::HashMap<String, (String, bool)> =
        std::collections::HashMap::new();
    for e in &entries {
        let id = e.downstream_key_id.as_str();
        if id.is_empty() || key_meta.contains_key(id) {
            continue;
        }
        match key_repo.get_any(id).await {
            Ok(Some(row)) => {
                key_meta.insert(id.to_string(), (row.name, row.deleted_at.is_some()));
            }
            Ok(None) => {
                key_meta.insert(id.to_string(), (String::new(), true));
            }
            Err(e) => return internal(e).into_response(),
        }
    }

    let provider_repo = ProviderRepo::new(&state.pool);
    let mut provider_meta: std::collections::HashMap<String, (String, bool)> =
        std::collections::HashMap::new();
    for p in &by_provider {
        let id = p.provider_id.as_str();
        if id.is_empty() || id == "(unknown)" || provider_meta.contains_key(id) {
            continue;
        }
        match provider_repo.get_any(id).await {
            Ok(Some(row)) => {
                provider_meta.insert(id.to_string(), (row.name, row.deleted_at.is_some()));
            }
            Ok(None) => {
                provider_meta.insert(id.to_string(), (String::new(), true));
            }
            Err(e) => return internal(e).into_response(),
        }
    }

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
            "entries": entries.iter().map(|e| {
                let (name, deleted) = key_meta
                    .get(&e.downstream_key_id)
                    .cloned()
                    .unwrap_or_else(|| (String::new(), !e.downstream_key_id.is_empty()));
                json!({
                    "downstream_key_id": e.downstream_key_id,
                    "name": name,
                    "deleted": deleted,
                    "request_count": e.request_count,
                    "total_usd": e.total_usd,
                    "prompt_tokens": e.prompt_tokens,
                    "completion_tokens": e.completion_tokens,
                    "total_tokens": e.total_tokens,
                })
            }).collect::<Vec<_>>(),
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
            "by_provider": by_provider.iter().map(|p| {
                let (name, deleted) = provider_meta
                    .get(&p.provider_id)
                    .cloned()
                    .unwrap_or_else(|| {
                        let gone = !p.provider_id.is_empty() && p.provider_id != "(unknown)";
                        (String::new(), gone)
                    });
                json!({
                    "provider_id": p.provider_id,
                    "name": name,
                    "deleted": deleted,
                    "provider_kind": p.provider_kind,
                    "request_count": p.request_count,
                    "success_count": p.success_count,
                    "success_rate": p.success_rate,
                    "avg_ttfb_ms": p.avg_ttfb_ms,
                    "avg_duration_ms": p.avg_duration_ms,
                    "tokens_per_sec": p.tokens_per_sec,
                    "total_usd": p.total_usd,
                    "total_tokens": p.total_tokens,
                })
            }).collect::<Vec<_>>(),
        })),
    )
        .into_response()
}

