//! Row insert helpers, SQLite row mapping, and factory constructors.

use chrono::Utc;
use sqlx::Row;

use crate::{
    schema::{UsageAttemptRow, UsageRecordRow},
    StoreError,
};

impl UsageRecordRow {
    /// A row with a freshly minted `id` + `ts` and the ledger's semantic
    /// defaults (`status = "ok"`, `attempt_count = 1`, everything else empty).
    ///
    /// Callers fill the fields they know via struct-update
    /// (`UsageRecordRow { request_id, .., ..UsageRecordRow::stamped() }`),
    /// so adding a ledger column only touches this one default site.
    pub fn stamped() -> Self {
        UsageRecordRow {
            id: ulid::Ulid::new().to_string(),
            ts: Utc::now().to_rfc3339(),
            request_id: String::new(),
            downstream_key_id: None,
            alias: None,
            provider_id: None,
            provider_kind: None,
            model_id: None,
            prompt_tokens: 0,
            completion_tokens: 0,
            total_tokens: 0,
            reasoning_tokens: 0,
            cache_read_tokens: 0,
            cache_write_tokens: 0,
            cost_usd: 0.0,
            stream: false,
            status: "ok".into(),
            error_class: None,
            http_status: None,
            finish_reason: None,
            duration_ms: None,
            ttfb_ms: None,
            route_strategy: None,
            attempt_no: 0,
            attempt_count: 1,
            session_id: None,
            affinity_hit: None,
            pool_id: None,
            selected_reason: None,
            loss_count: 0,
            wire_format: None,
        }
    }
}

pub(crate) async fn insert_row_on<'e, E>(ex: E, row: &UsageRecordRow) -> Result<(), StoreError>
where
    E: sqlx::Executor<'e, Database = sqlx::Sqlite>,
{
    sqlx::query(
        r#"INSERT INTO usage_records (
               id, ts, request_id, downstream_key_id, alias,
               provider_id, provider_kind, model_id,
               prompt_tokens, completion_tokens, total_tokens,
               reasoning_tokens, cache_read_tokens, cache_write_tokens,
               cost_usd, stream,
               status, error_class, http_status, finish_reason,
               duration_ms, ttfb_ms, route_strategy,
               attempt_no, attempt_count, session_id, affinity_hit,
               pool_id, selected_reason, loss_count, wire_format
           ) VALUES (
               ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?,
               ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?
           )"#,
    )
    .bind(&row.id)
    .bind(&row.ts)
    .bind(&row.request_id)
    .bind(&row.downstream_key_id)
    .bind(&row.alias)
    .bind(&row.provider_id)
    .bind(&row.provider_kind)
    .bind(&row.model_id)
    .bind(row.prompt_tokens)
    .bind(row.completion_tokens)
    .bind(row.total_tokens)
    .bind(row.reasoning_tokens)
    .bind(row.cache_read_tokens)
    .bind(row.cache_write_tokens)
    .bind(row.cost_usd)
    .bind(row.stream as i32)
    .bind(&row.status)
    .bind(&row.error_class)
    .bind(row.http_status.map(|s| s as i64))
    .bind(&row.finish_reason)
    .bind(row.duration_ms.map(|d| d as i64))
    .bind(row.ttfb_ms.map(|d| d as i64))
    .bind(&row.route_strategy)
    .bind(row.attempt_no as i64)
    .bind(row.attempt_count as i64)
    .bind(&row.session_id)
    .bind(row.affinity_hit.map(|b| b as i32))
    .bind(&row.pool_id)
    .bind(&row.selected_reason)
    .bind(row.loss_count as i64)
    .bind(&row.wire_format)
    .execute(ex)
    .await
    .map_err(|e| StoreError::Sqlx(e.to_string()))?;
    Ok(())
}

pub(crate) async fn insert_attempt_on<'e, E>(ex: E, row: &UsageAttemptRow) -> Result<(), StoreError>
where
    E: sqlx::Executor<'e, Database = sqlx::Sqlite>,
{
    sqlx::query(
        r#"INSERT INTO usage_attempts (
               id, request_id, attempt_no, provider_id, provider_kind, model_id,
               status, error_class, http_status, duration_ms, ttfb_ms, reason, ts
           ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)"#,
    )
    .bind(&row.id)
    .bind(&row.request_id)
    .bind(row.attempt_no as i64)
    .bind(&row.provider_id)
    .bind(&row.provider_kind)
    .bind(&row.model_id)
    .bind(&row.status)
    .bind(&row.error_class)
    .bind(row.http_status.map(|s| s as i64))
    .bind(row.duration_ms.map(|d| d as i64))
    .bind(row.ttfb_ms.map(|d| d as i64))
    .bind(&row.reason)
    .bind(&row.ts)
    .execute(ex)
    .await
    .map_err(|e| StoreError::Sqlx(e.to_string()))?;
    Ok(())
}

pub(crate) fn map_row(r: sqlx::sqlite::SqliteRow) -> UsageRecordRow {
    UsageRecordRow {
        id: r.get("id"),
        ts: r.get("ts"),
        request_id: r.get("request_id"),
        downstream_key_id: r.get("downstream_key_id"),
        alias: r.get("alias"),
        provider_id: r.get("provider_id"),
        provider_kind: r.get("provider_kind"),
        model_id: r.get("model_id"),
        prompt_tokens: r.get::<i64, _>("prompt_tokens") as u32,
        completion_tokens: r.get::<i64, _>("completion_tokens") as u32,
        total_tokens: r.get::<i64, _>("total_tokens") as u32,
        reasoning_tokens: r.get::<i64, _>("reasoning_tokens") as u32,
        cache_read_tokens: r.get::<i64, _>("cache_read_tokens") as u32,
        cache_write_tokens: r.get::<i64, _>("cache_write_tokens") as u32,
        cost_usd: r.get("cost_usd"),
        stream: r.get::<i32, _>("stream") != 0,
        status: r
            .try_get::<String, _>("status")
            .unwrap_or_else(|_| "ok".into()),
        error_class: r.try_get("error_class").ok().flatten(),
        http_status: r
            .try_get::<Option<i64>, _>("http_status")
            .ok()
            .flatten()
            .map(|s| s as u16),
        finish_reason: r.try_get("finish_reason").ok().flatten(),
        duration_ms: r
            .try_get::<Option<i64>, _>("duration_ms")
            .ok()
            .flatten()
            .map(|d| d as u64),
        ttfb_ms: r
            .try_get::<Option<i64>, _>("ttfb_ms")
            .ok()
            .flatten()
            .map(|d| d as u64),
        route_strategy: r.try_get("route_strategy").ok().flatten(),
        attempt_no: r.try_get::<i64, _>("attempt_no").unwrap_or(0) as u32,
        attempt_count: r.try_get::<i64, _>("attempt_count").unwrap_or(1) as u32,
        session_id: r.try_get("session_id").ok().flatten(),
        affinity_hit: r
            .try_get::<Option<i32>, _>("affinity_hit")
            .ok()
            .flatten()
            .map(|b| b != 0),
        pool_id: r.try_get("pool_id").ok().flatten(),
        selected_reason: r.try_get("selected_reason").ok().flatten(),
        loss_count: r.try_get::<i64, _>("loss_count").unwrap_or(0) as u32,
        wire_format: r.try_get("wire_format").ok().flatten(),
    }
}

pub(crate) fn map_attempt_row(r: sqlx::sqlite::SqliteRow) -> UsageAttemptRow {
    UsageAttemptRow {
        id: r.get("id"),
        request_id: r.get("request_id"),
        attempt_no: r.get::<i64, _>("attempt_no") as u32,
        provider_id: r.get("provider_id"),
        provider_kind: r.get("provider_kind"),
        model_id: r.get("model_id"),
        status: r.get("status"),
        error_class: r.get("error_class"),
        http_status: r
            .get::<Option<i64>, _>("http_status")
            .map(|s| s as u16),
        duration_ms: r.get::<Option<i64>, _>("duration_ms").map(|d| d as u64),
        ttfb_ms: r.get::<Option<i64>, _>("ttfb_ms").map(|d| d as u64),
        reason: r.get("reason"),
        ts: r.get("ts"),
    }
}

/// Build a main ledger row. Generates `id` and `ts`.
#[allow(clippy::too_many_arguments)]
pub fn new_usage_record(
    request_id: impl Into<String>,
    downstream_key_id: Option<String>,
    alias: Option<String>,
    provider_id: Option<String>,
    provider_kind: Option<String>,
    model_id: Option<String>,
    prompt_tokens: u32,
    completion_tokens: u32,
    total_tokens: u32,
    reasoning_tokens: u32,
    cache_read_tokens: u32,
    cache_write_tokens: u32,
    cost_usd: f64,
    stream: bool,
) -> UsageRecordRow {
    UsageRecordRow {
        request_id: request_id.into(),
        downstream_key_id,
        alias,
        provider_id,
        provider_kind,
        model_id,
        prompt_tokens,
        completion_tokens,
        total_tokens,
        reasoning_tokens,
        cache_read_tokens,
        cache_write_tokens,
        cost_usd,
        stream,
        ..UsageRecordRow::stamped()
    }
}

/// Build an attempt row. Generates `id` and `ts`.
#[allow(clippy::too_many_arguments)]
pub fn new_usage_attempt(
    request_id: impl Into<String>,
    attempt_no: u32,
    provider_id: Option<String>,
    provider_kind: Option<String>,
    model_id: Option<String>,
    status: impl Into<String>,
    error_class: Option<String>,
    http_status: Option<u16>,
    duration_ms: Option<u64>,
    ttfb_ms: Option<u64>,
    reason: Option<String>,
) -> UsageAttemptRow {
    UsageAttemptRow {
        id: ulid::Ulid::new().to_string(),
        request_id: request_id.into(),
        attempt_no,
        provider_id,
        provider_kind,
        model_id,
        status: status.into(),
        error_class,
        http_status,
        duration_ms,
        ttfb_ms,
        reason,
        ts: Utc::now().to_rfc3339(),
    }
}

