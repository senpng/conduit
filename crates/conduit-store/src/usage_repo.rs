//! Per-request usage ledger.
//!
//! Every completed request with non-zero tokens or cost is written here.
//!
//! Calendar day / month rollups default to **UTC**, but accept a client
//! `tz_offset_minutes` (minutes east of UTC) so TUI charts follow local days.

use chrono::Utc;
use sqlx::{Row, SqlitePool};
use tracing::instrument;

use crate::{
    schema::{UsageAttemptRow, UsageRecordRow},
    StoreError,
};

/// Clamp client-provided offset to a sane range (minutes east of UTC).
pub fn clamp_tz_offset_minutes(minutes: i32) -> i32 {
    minutes.clamp(-14 * 60, 14 * 60)
}

/// SQLite `date(..., ?)` modifier, e.g. `+480 minutes` / `-300 minutes`.
fn offset_modifier(tz_offset_minutes: i32) -> String {
    format!("{:+} minutes", clamp_tz_offset_minutes(tz_offset_minutes))
}

/// Local calendar date of stored UTC RFC3339 `ts`:
/// `date(replace(substr(ts, 1, 19), 'T', ' '), "{:+} minutes")`
/// where `ts` is from `Utc::now().to_rfc3339()` (first 19 chars = `YYYY-MM-DDTHH:MM:SS`).

pub struct UsageRepo<'a> {
    pool: &'a SqlitePool,
}

/// Sort key for paginated usage list (always descending).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum UsageListSort {
    #[default]
    Date,
    Cost,
    Tokens,
}

impl UsageListSort {
    pub fn parse(s: &str) -> Self {
        match s.trim().to_ascii_lowercase().as_str() {
            "cost" => Self::Cost,
            "tokens" | "token" => Self::Tokens,
            _ => Self::Date,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Date => "date",
            Self::Cost => "cost",
            Self::Tokens => "tokens",
        }
    }
}

/// Options for [`UsageRepo::list_page`].
#[derive(Debug, Clone, Copy)]
pub struct UsageListOpts<'a> {
    pub limit: usize,
    pub offset: usize,
    pub key_id: Option<&'a str>,
    pub period: Option<&'a str>,
    /// Case-insensitive substring across model / alias / provider / request / key.
    pub q: Option<&'a str>,
    pub sort: UsageListSort,
    /// Client timezone offset minutes east of UTC (0 = UTC calendar).
    pub tz_offset_minutes: i32,
}

/// One page of usage rows plus total matching count.
#[derive(Debug, Clone)]
pub struct UsageListPage {
    pub rows: Vec<UsageRecordRow>,
    pub total: u64,
    pub limit: usize,
    pub offset: usize,
}

impl<'a> UsageRepo<'a> {
    pub fn new(pool: &'a SqlitePool) -> Self {
        Self { pool }
    }

    /// Insert one request ledger row (success, zero-token, or terminal failure).
    #[instrument(skip(self, row))]
    pub async fn insert(&self, row: &UsageRecordRow) -> Result<(), StoreError> {
        self.insert_ledger(row, &[]).await
    }

    /// Insert one per-try attempt row.
    #[instrument(skip(self, row))]
    pub async fn insert_attempt(&self, row: &UsageAttemptRow) -> Result<(), StoreError> {
        insert_attempt_on(self.pool, row).await
    }

    /// Insert a main row + optional attempts in a single transaction.
    #[instrument(skip(self, row, attempts))]
    pub async fn insert_ledger(
        &self,
        row: &UsageRecordRow,
        attempts: &[UsageAttemptRow],
    ) -> Result<(), StoreError> {
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| StoreError::Sqlx(e.to_string()))?;
        insert_row_on(&mut *tx, row).await?;
        for a in attempts {
            insert_attempt_on(&mut *tx, a).await?;
        }
        tx.commit()
            .await
            .map_err(|e| StoreError::Sqlx(e.to_string()))?;
        Ok(())
    }

    /// Insert many ledger entries in one transaction (async writer batching).
    #[instrument(skip(self, items))]
    pub async fn insert_ledger_batch(
        &self,
        items: &[(UsageRecordRow, Vec<UsageAttemptRow>)],
    ) -> Result<(), StoreError> {
        if items.is_empty() {
            return Ok(());
        }
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| StoreError::Sqlx(e.to_string()))?;
        for (row, attempts) in items {
            insert_row_on(&mut *tx, row).await?;
            for a in attempts {
                insert_attempt_on(&mut *tx, a).await?;
            }
        }
        tx.commit()
            .await
            .map_err(|e| StoreError::Sqlx(e.to_string()))?;
        Ok(())
    }

    /// List attempt rows for a request (ordered by attempt_no).
    #[instrument(skip(self))]
    pub async fn list_attempts(
        &self,
        request_id: &str,
    ) -> Result<Vec<UsageAttemptRow>, StoreError> {
        let rows = sqlx::query(
            r#"SELECT id, request_id, attempt_no, provider_id, provider_kind, model_id,
                      status, error_class, http_status, duration_ms, ttfb_ms, reason, ts
               FROM usage_attempts
               WHERE request_id = ?
               ORDER BY attempt_no ASC"#,
        )
        .bind(request_id)
        .fetch_all(self.pool)
        .await
        .map_err(|e| StoreError::Sqlx(e.to_string()))?;
        Ok(rows.into_iter().map(map_attempt_row).collect())
    }

    /// Recent usage rows (default: newest first). Thin wrapper around [`list_page`].
    #[instrument(skip(self))]
    pub async fn list(
        &self,
        limit: usize,
        key_id: Option<&str>,
        period: Option<&str>,
    ) -> Result<Vec<UsageRecordRow>, StoreError> {
        let page = self
            .list_page(UsageListOpts {
                limit,
                offset: 0,
                key_id,
                period,
                q: None,
                sort: UsageListSort::Date,
                        tz_offset_minutes: 0,
                        })
            .await?;
        Ok(page.rows)
    }

    /// Paginated usage list with optional key / period / free-text filter and sort.
    ///
    /// - `period` (`YYYY-MM`) scopes rows by **local** calendar month (`tz_offset_minutes`).
    /// - `key_id` scopes to one downstream key.
    /// - `q` matches (case-insensitive substring) model, alias, provider, request id, key id.
    /// - `sort` is always descending (date / cost / tokens), with `ts` as tie-breaker.
    #[instrument(skip(self))]
    pub async fn list_page(&self, opts: UsageListOpts<'_>) -> Result<UsageListPage, StoreError> {
        let limit = opts.limit.clamp(1, 500) as i64;
        let offset = opts.offset as i64;
        let key_id = opts.key_id.map(str::trim).filter(|s| !s.is_empty());
        let off = offset_modifier(opts.tz_offset_minutes);
        let period_pat = opts
            .period
            .map(str::trim)
            .filter(|p| !p.is_empty() && !p.eq_ignore_ascii_case("all"))
            .map(|p| format!("{p}%"));
        let q = opts
            .q
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(|s| s.to_lowercase())
            .unwrap_or_default();

        let order = match opts.sort {
            UsageListSort::Date => "ts DESC",
            UsageListSort::Cost => "cost_usd DESC, ts DESC",
            UsageListSort::Tokens => "total_tokens DESC, ts DESC",
        };

        // Period uses local calendar day; empty period_pat skips the predicate.
        // ?4 = timezone offset modifier for date().
        let count_sql = r#"
            SELECT COUNT(*) AS cnt
            FROM usage_records
            WHERE (?1 IS NULL OR downstream_key_id = ?1)
              AND (?2 IS NULL OR date(replace(substr(ts, 1, 19), 'T', ' '), ?4) LIKE ?2)
              AND (
                length(?3) = 0
                OR instr(lower(ifnull(model_id, '')), ?3) > 0
                OR instr(lower(ifnull(alias, '')), ?3) > 0
                OR instr(lower(ifnull(provider_kind, '')), ?3) > 0
                OR instr(lower(ifnull(provider_id, '')), ?3) > 0
                OR instr(lower(ifnull(request_id, '')), ?3) > 0
                OR instr(lower(ifnull(downstream_key_id, '')), ?3) > 0
              )
        "#;
        let total: i64 = sqlx::query_scalar(count_sql)
            .bind(key_id)
            .bind(period_pat.as_deref())
            .bind(&q)
            .bind(&off)
            .fetch_one(self.pool)
            .await
            .map_err(|e| StoreError::Sqlx(e.to_string()))?;

        let list_sql = format!(
            r#"
            SELECT id, ts, request_id, downstream_key_id, alias,
                   provider_id, provider_kind, model_id,
                   prompt_tokens, completion_tokens, total_tokens,
                   reasoning_tokens, cache_read_tokens, cache_write_tokens,
                   cost_usd, stream,
                   status, error_class, http_status, finish_reason,
                   duration_ms, ttfb_ms, route_strategy,
                   attempt_no, attempt_count, session_id, affinity_hit,
                   pool_id, selected_reason
            FROM usage_records
            WHERE (?1 IS NULL OR downstream_key_id = ?1)
              AND (?2 IS NULL OR date(replace(substr(ts, 1, 19), 'T', ' '), ?4) LIKE ?2)
              AND (
                length(?3) = 0
                OR instr(lower(ifnull(model_id, '')), ?3) > 0
                OR instr(lower(ifnull(alias, '')), ?3) > 0
                OR instr(lower(ifnull(provider_kind, '')), ?3) > 0
                OR instr(lower(ifnull(provider_id, '')), ?3) > 0
                OR instr(lower(ifnull(request_id, '')), ?3) > 0
                OR instr(lower(ifnull(downstream_key_id, '')), ?3) > 0
              )
            ORDER BY {order}
            LIMIT ?5 OFFSET ?6
            "#
        );
        let rows = sqlx::query(&list_sql)
            .bind(key_id)
            .bind(period_pat.as_deref())
            .bind(&q)
            .bind(&off)
            .bind(limit)
            .bind(offset)
            .fetch_all(self.pool)
            .await
            .map_err(|e| StoreError::Sqlx(e.to_string()))?;

        Ok(UsageListPage {
            rows: rows.into_iter().map(map_row).collect(),
            total: total.max(0) as u64,
            limit: opts.limit.clamp(1, 500),
            offset: opts.offset,
        })
    }

    /// Aggregate spend by downstream key.
    ///
    /// * `period` = `YYYY-MM` — local calendar month (`tz_offset_minutes`).
    /// * `period` = `"all"` — no time filter (lifetime totals).
    #[instrument(skip(self))]
    pub async fn summary_period(
        &self,
        period: &str,
        tz_offset_minutes: i32,
    ) -> Result<Vec<UsageSummaryRow>, StoreError> {
        let off = offset_modifier(tz_offset_minutes);
        let rows = match period_day_like(period) {
            Some(pattern) => {
                sqlx::query(
                    r#"SELECT
                   COALESCE(downstream_key_id, '') AS downstream_key_id,
                   COUNT(*) AS request_count,
                   COALESCE(SUM(cost_usd), 0) AS total_usd,
                   COALESCE(SUM(prompt_tokens), 0) AS prompt_tokens,
                   COALESCE(SUM(completion_tokens), 0) AS completion_tokens,
                   COALESCE(SUM(total_tokens), 0) AS total_tokens
               FROM usage_records
               WHERE date(replace(substr(ts, 1, 19), 'T', ' '), ?) LIKE ?
               GROUP BY COALESCE(downstream_key_id, '')
               ORDER BY total_usd DESC"#,
                )
                .bind(&off)
                .bind(&pattern)
                .fetch_all(self.pool)
                .await
            }
            None => {
                sqlx::query(
                    r#"SELECT
                   COALESCE(downstream_key_id, '') AS downstream_key_id,
                   COUNT(*) AS request_count,
                   COALESCE(SUM(cost_usd), 0) AS total_usd,
                   COALESCE(SUM(prompt_tokens), 0) AS prompt_tokens,
                   COALESCE(SUM(completion_tokens), 0) AS completion_tokens,
                   COALESCE(SUM(total_tokens), 0) AS total_tokens
               FROM usage_records
               GROUP BY COALESCE(downstream_key_id, '')
               ORDER BY total_usd DESC"#,
                )
                .fetch_all(self.pool)
                .await
            }
        }
        .map_err(|e| StoreError::Sqlx(e.to_string()))?;

        Ok(rows
            .into_iter()
            .map(|r| UsageSummaryRow {
                downstream_key_id: r.get("downstream_key_id"),
                request_count: r.get::<i64, _>("request_count") as u64,
                total_usd: r.get("total_usd"),
                prompt_tokens: r.get::<i64, _>("prompt_tokens") as u64,
                completion_tokens: r.get::<i64, _>("completion_tokens") as u64,
                total_tokens: r.get::<i64, _>("total_tokens") as u64,
            })
            .collect())
    }

    /// Daily rollup for a trailing window ending today (`since_day` = local `YYYY-MM-DD`).
    ///
    /// Used by the TUI contribution graph (≈52 weeks), independent of the
    /// selected calendar-month period cards.
    #[instrument(skip(self))]
    pub async fn summary_by_day_since(
        &self,
        since_day: &str,
        key_id: Option<&str>,
        tz_offset_minutes: i32,
    ) -> Result<Vec<UsageDayRow>, StoreError> {
        let off = offset_modifier(tz_offset_minutes);
        let rows = match key_id {
            Some(kid) => {
                sqlx::query(
                    r#"SELECT
                       date(replace(substr(ts, 1, 19), 'T', ' '), ?) AS day,
                       COUNT(*) AS request_count,
                       COALESCE(SUM(cost_usd), 0) AS total_usd,
                       COALESCE(SUM(total_tokens), 0) AS total_tokens
                   FROM usage_records
                   WHERE date(replace(substr(ts, 1, 19), 'T', ' '), ?) >= ?
                     AND downstream_key_id = ?
                   GROUP BY date(replace(substr(ts, 1, 19), 'T', ' '), ?)
                   ORDER BY day ASC"#,
                )
                .bind(&off)
                .bind(&off)
                .bind(since_day)
                .bind(kid)
                .bind(&off)
                .fetch_all(self.pool)
                .await
            }
            None => {
                sqlx::query(
                    r#"SELECT
                       date(replace(substr(ts, 1, 19), 'T', ' '), ?) AS day,
                       COUNT(*) AS request_count,
                       COALESCE(SUM(cost_usd), 0) AS total_usd,
                       COALESCE(SUM(total_tokens), 0) AS total_tokens
                   FROM usage_records
                   WHERE date(replace(substr(ts, 1, 19), 'T', ' '), ?) >= ?
                   GROUP BY date(replace(substr(ts, 1, 19), 'T', ' '), ?)
                   ORDER BY day ASC"#,
                )
                .bind(&off)
                .bind(&off)
                .bind(since_day)
                .bind(&off)
                .fetch_all(self.pool)
                .await
            }
        }
        .map_err(|e| StoreError::Sqlx(e.to_string()))?;

        Ok(rows
            .into_iter()
            .map(|r| UsageDayRow {
                day: r.get("day"),
                request_count: r.get::<i64, _>("request_count") as u64,
                total_usd: r.get("total_usd"),
                total_tokens: r.get::<i64, _>("total_tokens") as u64,
            })
            .collect())
    }

    /// Daily rollup for a calendar period (`YYYY-MM`) or all-time (`"all"`).
    ///
    /// Days are in the client local calendar (`tz_offset_minutes`). Optional
    /// `key_id` scopes to one downstream key.
    #[instrument(skip(self))]
    pub async fn summary_by_day(
        &self,
        period: &str,
        key_id: Option<&str>,
        tz_offset_minutes: i32,
    ) -> Result<Vec<UsageDayRow>, StoreError> {
        let off = offset_modifier(tz_offset_minutes);
        let pattern = period_day_like(period);
        let rows = match (pattern.as_deref(), key_id) {
            (Some(pat), Some(kid)) => {
                sqlx::query(
                    r#"SELECT
                       date(replace(substr(ts, 1, 19), 'T', ' '), ?) AS day,
                       COUNT(*) AS request_count,
                       COALESCE(SUM(cost_usd), 0) AS total_usd,
                       COALESCE(SUM(total_tokens), 0) AS total_tokens
                   FROM usage_records
                   WHERE date(replace(substr(ts, 1, 19), 'T', ' '), ?) LIKE ?
                     AND downstream_key_id = ?
                   GROUP BY date(replace(substr(ts, 1, 19), 'T', ' '), ?)
                   ORDER BY day ASC"#,
                )
                .bind(&off)
                .bind(&off)
                .bind(pat)
                .bind(kid)
                .bind(&off)
                .fetch_all(self.pool)
                .await
            }
            (Some(pat), None) => {
                sqlx::query(
                    r#"SELECT
                       date(replace(substr(ts, 1, 19), 'T', ' '), ?) AS day,
                       COUNT(*) AS request_count,
                       COALESCE(SUM(cost_usd), 0) AS total_usd,
                       COALESCE(SUM(total_tokens), 0) AS total_tokens
                   FROM usage_records
                   WHERE date(replace(substr(ts, 1, 19), 'T', ' '), ?) LIKE ?
                   GROUP BY date(replace(substr(ts, 1, 19), 'T', ' '), ?)
                   ORDER BY day ASC"#,
                )
                .bind(&off)
                .bind(&off)
                .bind(pat)
                .bind(&off)
                .fetch_all(self.pool)
                .await
            }
            (None, Some(kid)) => {
                sqlx::query(
                    r#"SELECT
                       date(replace(substr(ts, 1, 19), 'T', ' '), ?) AS day,
                       COUNT(*) AS request_count,
                       COALESCE(SUM(cost_usd), 0) AS total_usd,
                       COALESCE(SUM(total_tokens), 0) AS total_tokens
                   FROM usage_records
                   WHERE downstream_key_id = ?
                   GROUP BY date(replace(substr(ts, 1, 19), 'T', ' '), ?)
                   ORDER BY day ASC"#,
                )
                .bind(&off)
                .bind(kid)
                .bind(&off)
                .fetch_all(self.pool)
                .await
            }
            (None, None) => {
                sqlx::query(
                    r#"SELECT
                       date(replace(substr(ts, 1, 19), 'T', ' '), ?) AS day,
                       COUNT(*) AS request_count,
                       COALESCE(SUM(cost_usd), 0) AS total_usd,
                       COALESCE(SUM(total_tokens), 0) AS total_tokens
                   FROM usage_records
                   GROUP BY date(replace(substr(ts, 1, 19), 'T', ' '), ?)
                   ORDER BY day ASC"#,
                )
                .bind(&off)
                .bind(&off)
                .fetch_all(self.pool)
                .await
            }
        }
        .map_err(|e| StoreError::Sqlx(e.to_string()))?;

        Ok(rows
            .into_iter()
            .map(|r| UsageDayRow {
                day: r.get("day"),
                request_count: r.get::<i64, _>("request_count") as u64,
                total_usd: r.get("total_usd"),
                total_tokens: r.get::<i64, _>("total_tokens") as u64,
            })
            .collect())
    }

    /// Model/alias rollup for a calendar period (`YYYY-MM`) or all-time (`"all"`).
    #[instrument(skip(self))]
    pub async fn summary_by_model(
        &self,
        period: &str,
        key_id: Option<&str>,
        tz_offset_minutes: i32,
    ) -> Result<Vec<UsageModelRow>, StoreError> {
        let off = offset_modifier(tz_offset_minutes);
        let pattern = period_day_like(period);
        let rows = match (pattern.as_deref(), key_id) {
            (Some(pat), Some(kid)) => {
                sqlx::query(
                    r#"SELECT
                       COALESCE(NULLIF(alias, ''), NULLIF(model_id, ''), '(unknown)') AS label,
                       provider_kind,
                       COUNT(*) AS request_count,
                       COALESCE(SUM(cost_usd), 0) AS total_usd,
                       COALESCE(SUM(total_tokens), 0) AS total_tokens,
                       SUM(CASE WHEN duration_ms IS NOT NULL AND completion_tokens > 0
                                THEN completion_tokens ELSE 0 END) * 1000.0
                           / NULLIF(SUM(CASE WHEN duration_ms IS NOT NULL AND completion_tokens > 0
                               -- COALESCE(ttfb_ms,0) is required: SQLite's scalar MAX()
                               -- returns NULL if any argument is NULL, unlike agg MAX().
                               THEN MAX(duration_ms - COALESCE(ttfb_ms, 0), 0)
                               ELSE NULL END), 0) AS tokens_per_sec
                   FROM usage_records
                   WHERE date(replace(substr(ts, 1, 19), 'T', ' '), ?) LIKE ? AND downstream_key_id = ?
                   GROUP BY COALESCE(NULLIF(alias, ''), NULLIF(model_id, ''), '(unknown)'),
                            provider_kind
                   ORDER BY total_usd DESC"#,
                )
                .bind(&off)
                .bind(pat)
                .bind(kid)
                .fetch_all(self.pool)
                .await
            }
            (Some(pat), None) => {
                sqlx::query(
                    r#"SELECT
                       COALESCE(NULLIF(alias, ''), NULLIF(model_id, ''), '(unknown)') AS label,
                       provider_kind,
                       COUNT(*) AS request_count,
                       COALESCE(SUM(cost_usd), 0) AS total_usd,
                       COALESCE(SUM(total_tokens), 0) AS total_tokens,
                       SUM(CASE WHEN duration_ms IS NOT NULL AND completion_tokens > 0
                                THEN completion_tokens ELSE 0 END) * 1000.0
                           / NULLIF(SUM(CASE WHEN duration_ms IS NOT NULL AND completion_tokens > 0
                               -- COALESCE(ttfb_ms,0) is required: SQLite's scalar MAX()
                               -- returns NULL if any argument is NULL, unlike agg MAX().
                               THEN MAX(duration_ms - COALESCE(ttfb_ms, 0), 0)
                               ELSE NULL END), 0) AS tokens_per_sec
                   FROM usage_records
                   WHERE date(replace(substr(ts, 1, 19), 'T', ' '), ?) LIKE ?
                   GROUP BY COALESCE(NULLIF(alias, ''), NULLIF(model_id, ''), '(unknown)'),
                            provider_kind
                   ORDER BY total_usd DESC"#,
                )
                .bind(&off)
                .bind(pat)
                .fetch_all(self.pool)
                .await
            }
            (None, Some(kid)) => {
                sqlx::query(
                    r#"SELECT
                       COALESCE(NULLIF(alias, ''), NULLIF(model_id, ''), '(unknown)') AS label,
                       provider_kind,
                       COUNT(*) AS request_count,
                       COALESCE(SUM(cost_usd), 0) AS total_usd,
                       COALESCE(SUM(total_tokens), 0) AS total_tokens,
                       SUM(CASE WHEN duration_ms IS NOT NULL AND completion_tokens > 0
                                THEN completion_tokens ELSE 0 END) * 1000.0
                           / NULLIF(SUM(CASE WHEN duration_ms IS NOT NULL AND completion_tokens > 0
                               -- COALESCE(ttfb_ms,0) is required: SQLite's scalar MAX()
                               -- returns NULL if any argument is NULL, unlike agg MAX().
                               THEN MAX(duration_ms - COALESCE(ttfb_ms, 0), 0)
                               ELSE NULL END), 0) AS tokens_per_sec
                   FROM usage_records
                   WHERE downstream_key_id = ?
                   GROUP BY COALESCE(NULLIF(alias, ''), NULLIF(model_id, ''), '(unknown)'),
                            provider_kind
                   ORDER BY total_usd DESC"#,
                )
                .bind(kid)
                .fetch_all(self.pool)
                .await
            }
            (None, None) => {
                sqlx::query(
                    r#"SELECT
                       COALESCE(NULLIF(alias, ''), NULLIF(model_id, ''), '(unknown)') AS label,
                       provider_kind,
                       COUNT(*) AS request_count,
                       COALESCE(SUM(cost_usd), 0) AS total_usd,
                       COALESCE(SUM(total_tokens), 0) AS total_tokens,
                       SUM(CASE WHEN duration_ms IS NOT NULL AND completion_tokens > 0
                                THEN completion_tokens ELSE 0 END) * 1000.0
                           / NULLIF(SUM(CASE WHEN duration_ms IS NOT NULL AND completion_tokens > 0
                               -- COALESCE(ttfb_ms,0) is required: SQLite's scalar MAX()
                               -- returns NULL if any argument is NULL, unlike agg MAX().
                               THEN MAX(duration_ms - COALESCE(ttfb_ms, 0), 0)
                               ELSE NULL END), 0) AS tokens_per_sec
                   FROM usage_records
                   GROUP BY COALESCE(NULLIF(alias, ''), NULLIF(model_id, ''), '(unknown)'),
                            provider_kind
                   ORDER BY total_usd DESC"#,
                )
                .fetch_all(self.pool)
                .await
            }
        }
        .map_err(|e| StoreError::Sqlx(e.to_string()))?;

        Ok(rows
            .into_iter()
            .map(|r| UsageModelRow {
                label: r.get("label"),
                provider_kind: r.get("provider_kind"),
                request_count: r.get::<i64, _>("request_count") as u64,
                total_usd: r.get("total_usd"),
                total_tokens: r.get::<i64, _>("total_tokens") as u64,
                tokens_per_sec: r.get::<Option<f64>, _>("tokens_per_sec"),
            })
            .collect())
    }

    /// Model breakdown nested under each downstream key for a period / all-time.
    #[instrument(skip(self))]
    pub async fn summary_by_key_model(
        &self,
        period: &str,
        tz_offset_minutes: i32,
    ) -> Result<Vec<UsageKeyModelRow>, StoreError> {
        let off = offset_modifier(tz_offset_minutes);
        let rows = match period_day_like(period) {
            Some(pattern) => {
                sqlx::query(
                    r#"SELECT
                   COALESCE(downstream_key_id, '') AS downstream_key_id,
                   COALESCE(NULLIF(alias, ''), NULLIF(model_id, ''), '(unknown)') AS label,
                   provider_kind,
                   COUNT(*) AS request_count,
                   COALESCE(SUM(cost_usd), 0) AS total_usd,
                   COALESCE(SUM(total_tokens), 0) AS total_tokens
               FROM usage_records
               WHERE date(replace(substr(ts, 1, 19), 'T', ' '), ?) LIKE ?
               GROUP BY COALESCE(downstream_key_id, ''),
                        COALESCE(NULLIF(alias, ''), NULLIF(model_id, ''), '(unknown)'),
                        provider_kind
               ORDER BY total_usd DESC"#,
                )
                .bind(&off)
                .bind(&pattern)
                .fetch_all(self.pool)
                .await
            }
            None => {
                sqlx::query(
                    r#"SELECT
                   COALESCE(downstream_key_id, '') AS downstream_key_id,
                   COALESCE(NULLIF(alias, ''), NULLIF(model_id, ''), '(unknown)') AS label,
                   provider_kind,
                   COUNT(*) AS request_count,
                   COALESCE(SUM(cost_usd), 0) AS total_usd,
                   COALESCE(SUM(total_tokens), 0) AS total_tokens
               FROM usage_records
               GROUP BY COALESCE(downstream_key_id, ''),
                        COALESCE(NULLIF(alias, ''), NULLIF(model_id, ''), '(unknown)'),
                        provider_kind
               ORDER BY total_usd DESC"#,
                )
                .fetch_all(self.pool)
                .await
            }
        }
        .map_err(|e| StoreError::Sqlx(e.to_string()))?;

        Ok(rows
            .into_iter()
            .map(|r| UsageKeyModelRow {
                downstream_key_id: r.get("downstream_key_id"),
                label: r.get("label"),
                provider_kind: r.get("provider_kind"),
                request_count: r.get::<i64, _>("request_count") as u64,
                total_usd: r.get("total_usd"),
                total_tokens: r.get::<i64, _>("total_tokens") as u64,
            })
            .collect())
    }

    /// Model breakdown nested under each local day for a period / all-time.
    #[instrument(skip(self))]
    pub async fn summary_by_day_model(
        &self,
        period: &str,
        tz_offset_minutes: i32,
    ) -> Result<Vec<UsageDayModelRow>, StoreError> {
        let off = offset_modifier(tz_offset_minutes);
        let rows = match period_day_like(period) {
            Some(pattern) => {
                sqlx::query(
                    r#"SELECT
                   date(replace(substr(ts, 1, 19), 'T', ' '), ?) AS day,
                   COALESCE(NULLIF(alias, ''), NULLIF(model_id, ''), '(unknown)') AS label,
                   provider_kind,
                   COUNT(*) AS request_count,
                   COALESCE(SUM(cost_usd), 0) AS total_usd,
                   COALESCE(SUM(total_tokens), 0) AS total_tokens
               FROM usage_records
               WHERE date(replace(substr(ts, 1, 19), 'T', ' '), ?) LIKE ?
               GROUP BY date(replace(substr(ts, 1, 19), 'T', ' '), ?),
                        COALESCE(NULLIF(alias, ''), NULLIF(model_id, ''), '(unknown)'),
                        provider_kind
               ORDER BY day ASC, total_usd DESC"#,
                )
                .bind(&off)
                .bind(&off)
                .bind(&pattern)
                .bind(&off)
                .fetch_all(self.pool)
                .await
            }
            None => {
                sqlx::query(
                    r#"SELECT
                   date(replace(substr(ts, 1, 19), 'T', ' '), ?) AS day,
                   COALESCE(NULLIF(alias, ''), NULLIF(model_id, ''), '(unknown)') AS label,
                   provider_kind,
                   COUNT(*) AS request_count,
                   COALESCE(SUM(cost_usd), 0) AS total_usd,
                   COALESCE(SUM(total_tokens), 0) AS total_tokens
               FROM usage_records
               GROUP BY date(replace(substr(ts, 1, 19), 'T', ' '), ?),
                        COALESCE(NULLIF(alias, ''), NULLIF(model_id, ''), '(unknown)'),
                        provider_kind
               ORDER BY day ASC, total_usd DESC"#,
                )
                .bind(&off)
                .bind(&off)
                .fetch_all(self.pool)
                .await
            }
        }
        .map_err(|e| StoreError::Sqlx(e.to_string()))?;

        Ok(rows
            .into_iter()
            .map(|r| UsageDayModelRow {
                day: r.get("day"),
                label: r.get("label"),
                provider_kind: r.get("provider_kind"),
                request_count: r.get::<i64, _>("request_count") as u64,
                total_usd: r.get("total_usd"),
                total_tokens: r.get::<i64, _>("total_tokens") as u64,
            })
            .collect())
    }

    /// Outcome / latency aggregates for a period or all-time (`"all"`).
    #[instrument(skip(self))]
    pub async fn summary_outcome(
        &self,
        period: &str,
        key_id: Option<&str>,
        tz_offset_minutes: i32,
    ) -> Result<UsageOutcomeSummary, StoreError> {
        let off = offset_modifier(tz_offset_minutes);
        let pattern = period_day_like(period);
        let row = match (pattern.as_deref(), key_id) {
            (Some(pat), Some(kid)) => {
                sqlx::query(
                    r#"SELECT
                           COUNT(*) AS request_count,
                           COALESCE(SUM(CASE WHEN status = 'ok' THEN 1 ELSE 0 END), 0)
                               AS success_count,
                           AVG(ttfb_ms) AS avg_ttfb_ms,
                           AVG(duration_ms) AS avg_duration_ms,
                           SUM(CASE WHEN duration_ms IS NOT NULL AND completion_tokens > 0
                                    THEN completion_tokens ELSE 0 END) * 1000.0
                               / NULLIF(SUM(CASE WHEN duration_ms IS NOT NULL AND completion_tokens > 0
                                   -- COALESCE(ttfb_ms,0) is required: SQLite's scalar MAX()
                                   -- returns NULL if any argument is NULL, unlike agg MAX().
                                   THEN MAX(duration_ms - COALESCE(ttfb_ms, 0), 0)
                                   ELSE NULL END), 0) AS tokens_per_sec
                       FROM usage_records
                       WHERE date(replace(substr(ts, 1, 19), 'T', ' '), ?) LIKE ? AND downstream_key_id = ?"#,
                )
                .bind(&off)
                .bind(pat)
                .bind(kid)
                .fetch_one(self.pool)
                .await
            }
            (Some(pat), None) => {
                sqlx::query(
                    r#"SELECT
                           COUNT(*) AS request_count,
                           COALESCE(SUM(CASE WHEN status = 'ok' THEN 1 ELSE 0 END), 0)
                               AS success_count,
                           AVG(ttfb_ms) AS avg_ttfb_ms,
                           AVG(duration_ms) AS avg_duration_ms,
                           SUM(CASE WHEN duration_ms IS NOT NULL AND completion_tokens > 0
                                    THEN completion_tokens ELSE 0 END) * 1000.0
                               / NULLIF(SUM(CASE WHEN duration_ms IS NOT NULL AND completion_tokens > 0
                                   -- COALESCE(ttfb_ms,0) is required: SQLite's scalar MAX()
                                   -- returns NULL if any argument is NULL, unlike agg MAX().
                                   THEN MAX(duration_ms - COALESCE(ttfb_ms, 0), 0)
                                   ELSE NULL END), 0) AS tokens_per_sec
                       FROM usage_records
                       WHERE date(replace(substr(ts, 1, 19), 'T', ' '), ?) LIKE ?"#,
                )
                .bind(&off)
                .bind(pat)
                .fetch_one(self.pool)
                .await
            }
            (None, Some(kid)) => {
                sqlx::query(
                    r#"SELECT
                           COUNT(*) AS request_count,
                           COALESCE(SUM(CASE WHEN status = 'ok' THEN 1 ELSE 0 END), 0)
                               AS success_count,
                           AVG(ttfb_ms) AS avg_ttfb_ms,
                           AVG(duration_ms) AS avg_duration_ms,
                           SUM(CASE WHEN duration_ms IS NOT NULL AND completion_tokens > 0
                                    THEN completion_tokens ELSE 0 END) * 1000.0
                               / NULLIF(SUM(CASE WHEN duration_ms IS NOT NULL AND completion_tokens > 0
                                   -- COALESCE(ttfb_ms,0) is required: SQLite's scalar MAX()
                                   -- returns NULL if any argument is NULL, unlike agg MAX().
                                   THEN MAX(duration_ms - COALESCE(ttfb_ms, 0), 0)
                                   ELSE NULL END), 0) AS tokens_per_sec
                       FROM usage_records
                       WHERE downstream_key_id = ?"#,
                )
                .bind(kid)
                .fetch_one(self.pool)
                .await
            }
            (None, None) => {
                sqlx::query(
                    r#"SELECT
                           COUNT(*) AS request_count,
                           COALESCE(SUM(CASE WHEN status = 'ok' THEN 1 ELSE 0 END), 0)
                               AS success_count,
                           AVG(ttfb_ms) AS avg_ttfb_ms,
                           AVG(duration_ms) AS avg_duration_ms,
                           SUM(CASE WHEN duration_ms IS NOT NULL AND completion_tokens > 0
                                    THEN completion_tokens ELSE 0 END) * 1000.0
                               / NULLIF(SUM(CASE WHEN duration_ms IS NOT NULL AND completion_tokens > 0
                                   -- COALESCE(ttfb_ms,0) is required: SQLite's scalar MAX()
                                   -- returns NULL if any argument is NULL, unlike agg MAX().
                                   THEN MAX(duration_ms - COALESCE(ttfb_ms, 0), 0)
                                   ELSE NULL END), 0) AS tokens_per_sec
                       FROM usage_records"#,
                )
                .fetch_one(self.pool)
                .await
            }
        }
        .map_err(|e| StoreError::Sqlx(e.to_string()))?;

        let request_count = row.get::<i64, _>("request_count").max(0) as u64;
        let success_count = row.get::<i64, _>("success_count").max(0) as u64;
        let success_rate = if request_count == 0 {
            0.0
        } else {
            success_count as f64 / request_count as f64
        };
        Ok(UsageOutcomeSummary {
            request_count,
            success_count,
            success_rate,
            avg_ttfb_ms: row.get::<Option<f64>, _>("avg_ttfb_ms"),
            avg_duration_ms: row.get::<Option<f64>, _>("avg_duration_ms"),
            tokens_per_sec: row.get::<Option<f64>, _>("tokens_per_sec"),
        })
    }

    /// Provider health rollup for a period or all-time (`"all"`).
    #[instrument(skip(self))]
    pub async fn summary_by_provider(
        &self,
        period: &str,
        key_id: Option<&str>,
        tz_offset_minutes: i32,
    ) -> Result<Vec<UsageProviderRow>, StoreError> {
        let off = offset_modifier(tz_offset_minutes);
        let pattern = period_day_like(period);
        let rows = match (pattern.as_deref(), key_id) {
            (Some(pat), Some(kid)) => {
                sqlx::query(
                    r#"SELECT
                       COALESCE(NULLIF(provider_id, ''), '(unknown)') AS provider_id,
                       provider_kind,
                       COUNT(*) AS request_count,
                       COALESCE(SUM(CASE WHEN status = 'ok' THEN 1 ELSE 0 END), 0)
                           AS success_count,
                       AVG(ttfb_ms) AS avg_ttfb_ms,
                       AVG(duration_ms) AS avg_duration_ms,
                       SUM(CASE WHEN duration_ms IS NOT NULL AND completion_tokens > 0
                                THEN completion_tokens ELSE 0 END) * 1000.0
                           / NULLIF(SUM(CASE WHEN duration_ms IS NOT NULL AND completion_tokens > 0
                               -- COALESCE(ttfb_ms,0) is required: SQLite's scalar MAX()
                               -- returns NULL if any argument is NULL, unlike agg MAX().
                               THEN MAX(duration_ms - COALESCE(ttfb_ms, 0), 0)
                               ELSE NULL END), 0) AS tokens_per_sec,
                       COALESCE(SUM(cost_usd), 0) AS total_usd,
                       COALESCE(SUM(total_tokens), 0) AS total_tokens
                   FROM usage_records
                   WHERE date(replace(substr(ts, 1, 19), 'T', ' '), ?) LIKE ? AND downstream_key_id = ?
                   GROUP BY COALESCE(NULLIF(provider_id, ''), '(unknown)'), provider_kind
                   ORDER BY total_tokens DESC"#,
                )
                .bind(&off)
                .bind(pat)
                .bind(kid)
                .fetch_all(self.pool)
                .await
            }
            (Some(pat), None) => {
                sqlx::query(
                    r#"SELECT
                       COALESCE(NULLIF(provider_id, ''), '(unknown)') AS provider_id,
                       provider_kind,
                       COUNT(*) AS request_count,
                       COALESCE(SUM(CASE WHEN status = 'ok' THEN 1 ELSE 0 END), 0)
                           AS success_count,
                       AVG(ttfb_ms) AS avg_ttfb_ms,
                       AVG(duration_ms) AS avg_duration_ms,
                       SUM(CASE WHEN duration_ms IS NOT NULL AND completion_tokens > 0
                                THEN completion_tokens ELSE 0 END) * 1000.0
                           / NULLIF(SUM(CASE WHEN duration_ms IS NOT NULL AND completion_tokens > 0
                               -- COALESCE(ttfb_ms,0) is required: SQLite's scalar MAX()
                               -- returns NULL if any argument is NULL, unlike agg MAX().
                               THEN MAX(duration_ms - COALESCE(ttfb_ms, 0), 0)
                               ELSE NULL END), 0) AS tokens_per_sec,
                       COALESCE(SUM(cost_usd), 0) AS total_usd,
                       COALESCE(SUM(total_tokens), 0) AS total_tokens
                   FROM usage_records
                   WHERE date(replace(substr(ts, 1, 19), 'T', ' '), ?) LIKE ?
                   GROUP BY COALESCE(NULLIF(provider_id, ''), '(unknown)'), provider_kind
                   ORDER BY total_tokens DESC"#,
                )
                .bind(&off)
                .bind(pat)
                .fetch_all(self.pool)
                .await
            }
            (None, Some(kid)) => {
                sqlx::query(
                    r#"SELECT
                       COALESCE(NULLIF(provider_id, ''), '(unknown)') AS provider_id,
                       provider_kind,
                       COUNT(*) AS request_count,
                       COALESCE(SUM(CASE WHEN status = 'ok' THEN 1 ELSE 0 END), 0)
                           AS success_count,
                       AVG(ttfb_ms) AS avg_ttfb_ms,
                       AVG(duration_ms) AS avg_duration_ms,
                       SUM(CASE WHEN duration_ms IS NOT NULL AND completion_tokens > 0
                                THEN completion_tokens ELSE 0 END) * 1000.0
                           / NULLIF(SUM(CASE WHEN duration_ms IS NOT NULL AND completion_tokens > 0
                               -- COALESCE(ttfb_ms,0) is required: SQLite's scalar MAX()
                               -- returns NULL if any argument is NULL, unlike agg MAX().
                               THEN MAX(duration_ms - COALESCE(ttfb_ms, 0), 0)
                               ELSE NULL END), 0) AS tokens_per_sec,
                       COALESCE(SUM(cost_usd), 0) AS total_usd,
                       COALESCE(SUM(total_tokens), 0) AS total_tokens
                   FROM usage_records
                   WHERE downstream_key_id = ?
                   GROUP BY COALESCE(NULLIF(provider_id, ''), '(unknown)'), provider_kind
                   ORDER BY total_tokens DESC"#,
                )
                .bind(kid)
                .fetch_all(self.pool)
                .await
            }
            (None, None) => {
                sqlx::query(
                    r#"SELECT
                       COALESCE(NULLIF(provider_id, ''), '(unknown)') AS provider_id,
                       provider_kind,
                       COUNT(*) AS request_count,
                       COALESCE(SUM(CASE WHEN status = 'ok' THEN 1 ELSE 0 END), 0)
                           AS success_count,
                       AVG(ttfb_ms) AS avg_ttfb_ms,
                       AVG(duration_ms) AS avg_duration_ms,
                       SUM(CASE WHEN duration_ms IS NOT NULL AND completion_tokens > 0
                                THEN completion_tokens ELSE 0 END) * 1000.0
                           / NULLIF(SUM(CASE WHEN duration_ms IS NOT NULL AND completion_tokens > 0
                               -- COALESCE(ttfb_ms,0) is required: SQLite's scalar MAX()
                               -- returns NULL if any argument is NULL, unlike agg MAX().
                               THEN MAX(duration_ms - COALESCE(ttfb_ms, 0), 0)
                               ELSE NULL END), 0) AS tokens_per_sec,
                       COALESCE(SUM(cost_usd), 0) AS total_usd,
                       COALESCE(SUM(total_tokens), 0) AS total_tokens
                   FROM usage_records
                   GROUP BY COALESCE(NULLIF(provider_id, ''), '(unknown)'), provider_kind
                   ORDER BY total_tokens DESC"#,
                )
                .fetch_all(self.pool)
                .await
            }
        }
        .map_err(|e| StoreError::Sqlx(e.to_string()))?;

        Ok(rows
            .into_iter()
            .map(|r| {
                let request_count = r.get::<i64, _>("request_count") as u64;
                let success_count = r.get::<i64, _>("success_count") as u64;
                let success_rate = if request_count == 0 {
                    0.0
                } else {
                    success_count as f64 / request_count as f64
                };
                UsageProviderRow {
                    provider_id: r.get("provider_id"),
                    provider_kind: r.get("provider_kind"),
                    request_count,
                    success_count,
                    success_rate,
                    avg_ttfb_ms: r.get::<Option<f64>, _>("avg_ttfb_ms"),
                    avg_duration_ms: r.get::<Option<f64>, _>("avg_duration_ms"),
                    tokens_per_sec: r.get::<Option<f64>, _>("tokens_per_sec"),
                    total_usd: r.get("total_usd"),
                    total_tokens: r.get::<i64, _>("total_tokens") as u64,
                }
            })
            .collect())
    }
}

/// `YYYY-MM%` for local calendar months; `None` for all-time (`period == "all"`).
fn period_day_like(period: &str) -> Option<String> {
    if period.eq_ignore_ascii_case("all") {
        None
    } else {
        // Local day is `YYYY-MM-DD`; prefix match scopes one calendar month.
        Some(format!("{}%", period.trim()))
    }
}

/// Period rollup used by console summary.
#[derive(Debug, Clone, PartialEq)]
pub struct UsageSummaryRow {
    pub downstream_key_id: String,
    pub request_count: u64,
    pub total_usd: f64,
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub total_tokens: u64,
}

/// One local calendar day within a period summary (`YYYY-MM-DD`).
#[derive(Debug, Clone, PartialEq)]
pub struct UsageDayRow {
    /// `YYYY-MM-DD` (UTC, from `ts` prefix).
    pub day: String,
    pub request_count: u64,
    pub total_usd: f64,
    pub total_tokens: u64,
}

/// One model/alias within a period summary.
#[derive(Debug, Clone, PartialEq)]
pub struct UsageModelRow {
    pub label: String,
    pub provider_kind: Option<String>,
    pub request_count: u64,
    pub total_usd: f64,
    pub total_tokens: u64,
    /// Throughput = Σcompletion_tokens / Σgeneration_ms, size-weighted — NOT a
    /// row-wise mean. `None` when no eligible row exists.
    pub tokens_per_sec: Option<f64>,
}

/// Model rollup for one downstream key within a period.
#[derive(Debug, Clone, PartialEq)]
pub struct UsageKeyModelRow {
    pub downstream_key_id: String,
    pub label: String,
    pub provider_kind: Option<String>,
    pub request_count: u64,
    pub total_usd: f64,
    pub total_tokens: u64,
}

/// Model rollup for one UTC day within a period.
#[derive(Debug, Clone, PartialEq)]
pub struct UsageDayModelRow {
    pub day: String,
    pub label: String,
    pub provider_kind: Option<String>,
    pub request_count: u64,
    pub total_usd: f64,
    pub total_tokens: u64,
}

/// Period outcome aggregates for success rate / latency cards.
#[derive(Debug, Clone, PartialEq)]
pub struct UsageOutcomeSummary {
    pub request_count: u64,
    pub success_count: u64,
    pub success_rate: f64,
    pub avg_ttfb_ms: Option<f64>,
    pub avg_duration_ms: Option<f64>,
    /// Throughput = Σcompletion_tokens / Σgeneration_ms, size-weighted — NOT a
    /// row-wise mean like `avg_ttfb_ms`. `None` when no eligible row exists.
    pub tokens_per_sec: Option<f64>,
}

/// Provider health rollup within a period.
#[derive(Debug, Clone, PartialEq)]
pub struct UsageProviderRow {
    pub provider_id: String,
    pub provider_kind: Option<String>,
    pub request_count: u64,
    pub success_count: u64,
    pub success_rate: f64,
    pub avg_ttfb_ms: Option<f64>,
    pub avg_duration_ms: Option<f64>,
    /// Throughput = Σcompletion_tokens / Σgeneration_ms, size-weighted — NOT a
    /// row-wise mean like `avg_ttfb_ms`. `None` when no eligible row exists.
    pub tokens_per_sec: Option<f64>,
    pub total_usd: f64,
    pub total_tokens: u64,
}

async fn insert_row_on<'e, E>(ex: E, row: &UsageRecordRow) -> Result<(), StoreError>
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
               pool_id, selected_reason
           ) VALUES (
               ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?,
               ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?
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
    .execute(ex)
    .await
    .map_err(|e| StoreError::Sqlx(e.to_string()))?;
    Ok(())
}

async fn insert_attempt_on<'e, E>(ex: E, row: &UsageAttemptRow) -> Result<(), StoreError>
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

fn map_row(r: sqlx::sqlite::SqliteRow) -> UsageRecordRow {
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
    }
}

fn map_attempt_row(r: sqlx::sqlite::SqliteRow) -> UsageAttemptRow {
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
        id: ulid::Ulid::new().to_string(),
        ts: Utc::now().to_rfc3339(),
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::open_db;

    #[tokio::test]
    async fn insert_and_list() {
        let pool = open_db("sqlite::memory:").await.unwrap();
        let repo = UsageRepo::new(&pool);

        let row = new_usage_record(
            "req-1",
            Some("dk1".into()),
            Some("gpt".into()),
            Some("p1".into()),
            Some("openai".into()),
            Some("gpt-4o".into()),
            10,
            5,
            15,
            0,
            0,
            0,
            0.012,
            false,
        );
        repo.insert(&row).await.unwrap();

        let listed = repo.list(10, None, None).await.unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].request_id, "req-1");
        assert!((listed[0].cost_usd - 0.012).abs() < 1e-9);

        let by_key = repo.list(10, Some("dk1"), None).await.unwrap();
        assert_eq!(by_key.len(), 1);
        let empty = repo.list(10, Some("other"), None).await.unwrap();
        assert!(empty.is_empty());

        let page = repo
            .list_page(UsageListOpts {
                limit: 10,
                offset: 0,
                key_id: None,
                period: None,
                q: Some("gpt-4o"),
                sort: UsageListSort::Date,
                        tz_offset_minutes: 0,
                        })
            .await
            .unwrap();
        assert_eq!(page.total, 1);
        assert_eq!(page.rows.len(), 1);
    }

    #[tokio::test]
    async fn list_page_offset_and_sort() {
        let pool = open_db("sqlite::memory:").await.unwrap();
        let repo = UsageRepo::new(&pool);

        for (i, (model, cost, toks)) in [
            ("cheap", 0.01, 10u32),
            ("mid", 0.50, 100),
            ("pricey", 2.00, 50),
        ]
        .into_iter()
        .enumerate()
        {
            let mut row = new_usage_record(
                &format!("req-{i}"),
                Some("dk1".into()),
                None,
                None,
                Some("openai".into()),
                Some(model.into()),
                toks,
                0,
                toks,
                0,
                0,
                0,
                cost,
                false,
            );
            // Distinct timestamps so date sort is stable.
            row.ts = format!("2026-07-{:02}T12:00:00Z", i + 1);
            repo.insert(&row).await.unwrap();
        }

        let by_cost = repo
            .list_page(UsageListOpts {
                limit: 2,
                offset: 0,
                key_id: None,
                period: Some("2026-07"),
                q: None,
                sort: UsageListSort::Cost,
                        tz_offset_minutes: 0,
                        })
            .await
            .unwrap();
        assert_eq!(by_cost.total, 3);
        assert_eq!(by_cost.rows.len(), 2);
        assert_eq!(by_cost.rows[0].model_id.as_deref(), Some("pricey"));
        assert_eq!(by_cost.rows[1].model_id.as_deref(), Some("mid"));

        let page2 = repo
            .list_page(UsageListOpts {
                limit: 2,
                offset: 2,
                key_id: None,
                period: Some("2026-07"),
                q: None,
                sort: UsageListSort::Cost,
                        tz_offset_minutes: 0,
                        })
            .await
            .unwrap();
        assert_eq!(page2.total, 3);
        assert_eq!(page2.rows.len(), 1);
        assert_eq!(page2.rows[0].model_id.as_deref(), Some("cheap"));

        let q = repo
            .list_page(UsageListOpts {
                limit: 10,
                offset: 0,
                key_id: None,
                period: None,
                q: Some("MID"),
                sort: UsageListSort::Date,
                        tz_offset_minutes: 0,
                        })
            .await
            .unwrap();
        assert_eq!(q.total, 1);
        assert_eq!(q.rows[0].model_id.as_deref(), Some("mid"));
    }

    #[tokio::test]
    async fn list_filters_by_period() {
        let pool = open_db("sqlite::memory:").await.unwrap();
        let repo = UsageRepo::new(&pool);

        let mut a = new_usage_record(
            "req-a",
            Some("dk1".into()),
            None,
            None,
            None,
            Some("m1".into()),
            1,
            1,
            2,
            0,
            0,
            0,
            0.01,
            false,
        );
        a.ts = "2026-06-15T12:00:00Z".into();
        repo.insert(&a).await.unwrap();

        let mut b = new_usage_record(
            "req-b",
            Some("dk1".into()),
            None,
            None,
            None,
            Some("m1".into()),
            1,
            1,
            2,
            0,
            0,
            0,
            0.02,
            false,
        );
        b.ts = "2026-07-01T08:00:00Z".into();
        repo.insert(&b).await.unwrap();

        let july = repo.list(10, None, Some("2026-07")).await.unwrap();
        assert_eq!(july.len(), 1);
        assert_eq!(july[0].request_id, "req-b");

        let june = repo.list(10, None, Some("2026-06")).await.unwrap();
        assert_eq!(june.len(), 1);
        assert_eq!(june[0].request_id, "req-a");

        let all = repo.list(10, None, None).await.unwrap();
        assert_eq!(all.len(), 2);
    }

    #[tokio::test]
    async fn summary_period_groups_by_key() {
        let pool = open_db("sqlite::memory:").await.unwrap();
        let repo = UsageRepo::new(&pool);

        let mut a = new_usage_record(
            "r1",
            Some("k1".into()),
            None,
            None,
            None,
            None,
            1,
            1,
            2,
            0,
            0,
            0,
            1.0,
            false,
        );
        a.ts = "2026-07-01T00:00:00Z".into();
        let mut b = new_usage_record(
            "r2",
            Some("k1".into()),
            None,
            None,
            None,
            None,
            2,
            2,
            4,
            0,
            0,
            0,
            2.5,
            true,
        );
        b.ts = "2026-07-15T12:00:00Z".into();
        let mut c = new_usage_record(
            "r3",
            Some("k2".into()),
            None,
            None,
            None,
            None,
            1,
            0,
            1,
            0,
            0,
            0,
            0.5,
            false,
        );
        c.ts = "2026-07-20T00:00:00Z".into();
        // Outside period
        let mut d = new_usage_record(
            "r4",
            Some("k1".into()),
            None,
            None,
            None,
            None,
            1,
            0,
            1,
            0,
            0,
            0,
            9.0,
            false,
        );
        d.ts = "2026-06-01T00:00:00Z".into();

        for r in [&a, &b, &c, &d] {
            repo.insert(r).await.unwrap();
        }

        let sum = repo.summary_period("2026-07", 0).await.unwrap();
        assert_eq!(sum.len(), 2);
        let k1 = sum.iter().find(|s| s.downstream_key_id == "k1").unwrap();
        assert_eq!(k1.request_count, 2);
        assert!((k1.total_usd - 3.5).abs() < 1e-9);
        assert_eq!(k1.total_tokens, 6);
        let k2 = sum.iter().find(|s| s.downstream_key_id == "k2").unwrap();
        assert_eq!(k2.request_count, 1);
        assert!((k2.total_usd - 0.5).abs() < 1e-9);

        // Lifetime includes the June outlier (k1 +$9).
        let all = repo.summary_period("all", 0).await.unwrap();
        assert_eq!(all.len(), 2);
        let k1_all = all.iter().find(|s| s.downstream_key_id == "k1").unwrap();
        assert_eq!(k1_all.request_count, 3);
        assert!((k1_all.total_usd - 12.5).abs() < 1e-9);
        assert_eq!(k1_all.total_tokens, 7);

        let by_day = repo.summary_by_day("2026-07", None, 0).await.unwrap();
        assert_eq!(by_day.len(), 3); // 01, 15, 20
        assert_eq!(by_day[0].day, "2026-07-01");
        assert!((by_day[0].total_usd - 1.0).abs() < 1e-9);
        let day15 = by_day.iter().find(|d| d.day == "2026-07-15").unwrap();
        assert!((day15.total_usd - 2.5).abs() < 1e-9);

        let k1_days = repo.summary_by_day("2026-07", Some("k1"), 0).await.unwrap();
        assert_eq!(k1_days.len(), 2);
        assert!(k1_days.iter().all(|d| d.day.starts_with("2026-07")));

        // Alias rollup (rows above have no alias — label falls back to "(unknown)")
        let by_model = repo.summary_by_model("2026-07", None, 0).await.unwrap();
        assert!(!by_model.is_empty());
        let unknown = by_model.iter().find(|m| m.label == "(unknown)").unwrap();
        assert_eq!(unknown.request_count, 3);
        assert!((unknown.total_usd - 4.0).abs() < 1e-9);
    }

    #[tokio::test]
    async fn summary_by_day_respects_tz_offset() {
        let pool = open_db("sqlite::memory:").await.unwrap();
        let repo = UsageRepo::new(&pool);
        // UTC 2026-07-31 20:00 → Asia/Shanghai (+480) is 2026-08-01 04:00
        let mut row = new_usage_record(
            "r-tz",
            Some("k1".into()),
            None,
            None,
            None,
            None,
            1,
            0,
            1,
            0,
            0,
            0,
            1.0,
            false,
        );
        row.ts = "2026-07-31T20:00:00Z".into();
        repo.insert(&row).await.unwrap();

        let utc_days = repo.summary_by_day("2026-07", None, 0).await.unwrap();
        assert_eq!(utc_days.len(), 1);
        assert_eq!(utc_days[0].day, "2026-07-31");

        let sh_days = repo.summary_by_day("2026-08", None, 480).await.unwrap();
        assert_eq!(sh_days.len(), 1);
        assert_eq!(sh_days[0].day, "2026-08-01");

        let sh_july = repo.summary_by_day("2026-07", None, 480).await.unwrap();
        assert!(sh_july.is_empty());
    }

    #[tokio::test]
    async fn zero_consumption_success_still_inserts() {
        let pool = open_db("sqlite::memory:").await.unwrap();
        let repo = UsageRepo::new(&pool);
        let mut row = new_usage_record(
            "req-zero",
            Some("dk1".into()),
            Some("gpt".into()),
            Some("p1".into()),
            Some("openai".into()),
            Some("gpt-4o".into()),
            0,
            0,
            0,
            0,
            0,
            0,
            0.0,
            false,
        );
        row.status = "ok".into();
        row.duration_ms = Some(12);
        repo.insert(&row).await.unwrap();
        let listed = repo.list(10, None, None).await.unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].status, "ok");
        assert_eq!(listed[0].duration_ms, Some(12));
        assert_eq!(listed[0].total_tokens, 0);
    }

    #[tokio::test]
    async fn terminal_error_and_attempts_insert() {
        let pool = open_db("sqlite::memory:").await.unwrap();
        let repo = UsageRepo::new(&pool);

        let mut main = new_usage_record(
            "req-err",
            Some("dk1".into()),
            Some("gpt".into()),
            Some("p2".into()),
            Some("openai".into()),
            Some("gpt-4o".into()),
            0,
            0,
            0,
            0,
            0,
            0,
            0.0,
            false,
        );
        main.status = "error".into();
        main.error_class = Some("rate_limited".into());
        main.http_status = Some(429);
        main.duration_ms = Some(80);
        main.attempt_no = 1;
        main.attempt_count = 2;
        main.route_strategy = Some("fallback".into());
        main.ts = "2026-07-15T10:00:00Z".into();
        repo.insert(&main).await.unwrap();

        let a0 = new_usage_attempt(
            "req-err",
            0,
            Some("p1".into()),
            Some("openai".into()),
            Some("gpt-4o".into()),
            "error",
            Some("rate_limited".into()),
            Some(429),
            Some(30),
            None,
            Some("initial".into()),
        );
        let a1 = new_usage_attempt(
            "req-err",
            1,
            Some("p2".into()),
            Some("openai".into()),
            Some("gpt-4o".into()),
            "error",
            Some("rate_limited".into()),
            Some(429),
            Some(50),
            None,
            Some("retry".into()),
        );
        repo.insert_attempt(&a0).await.unwrap();
        repo.insert_attempt(&a1).await.unwrap();

        let attempts = repo.list_attempts("req-err").await.unwrap();
        assert_eq!(attempts.len(), 2);
        assert_eq!(attempts[0].provider_id.as_deref(), Some("p1"));
        assert_eq!(attempts[1].provider_id.as_deref(), Some("p2"));
        assert_eq!(attempts[0].status, "error");

        let outcome = repo.summary_outcome("2026-07", None, 0).await.unwrap();
        assert_eq!(outcome.request_count, 1);
        assert_eq!(outcome.success_count, 0);
        assert!((outcome.success_rate - 0.0).abs() < 1e-12);

        let by_p = repo.summary_by_provider("2026-07", None, 0).await.unwrap();
        assert_eq!(by_p.len(), 1);
        assert_eq!(by_p[0].provider_id, "p2");
        assert!((by_p[0].success_rate - 0.0).abs() < 1e-12);
    }

    #[tokio::test]
    async fn summary_outcome_and_by_provider_success_rate() {
        let pool = open_db("sqlite::memory:").await.unwrap();
        let repo = UsageRepo::new(&pool);

        let mut ok = new_usage_record(
            "r-ok",
            Some("k1".into()),
            None,
            Some("prov-a".into()),
            Some("openai".into()),
            Some("m".into()),
            1,
            1,
            2,
            0,
            0,
            0,
            0.1,
            true,
        );
        ok.ts = "2026-07-10T00:00:00Z".into();
        ok.status = "ok".into();
        ok.ttfb_ms = Some(40);
        ok.duration_ms = Some(100);
        repo.insert(&ok).await.unwrap();

        let mut err = new_usage_record(
            "r-err",
            Some("k1".into()),
            None,
            Some("prov-a".into()),
            Some("openai".into()),
            Some("m".into()),
            0,
            0,
            0,
            0,
            0,
            0,
            0.0,
            false,
        );
        err.ts = "2026-07-11T00:00:00Z".into();
        err.status = "error".into();
        err.error_class = Some("upstream_5xx".into());
        err.duration_ms = Some(20);
        repo.insert(&err).await.unwrap();

        let mut ok_b = new_usage_record(
            "r-ok-b",
            Some("k1".into()),
            None,
            Some("prov-b".into()),
            Some("anthropic".into()),
            Some("m2".into()),
            2,
            2,
            4,
            0,
            0,
            0,
            0.2,
            false,
        );
        ok_b.ts = "2026-07-12T00:00:00Z".into();
        ok_b.status = "ok".into();
        ok_b.ttfb_ms = Some(80);
        ok_b.duration_ms = Some(200);
        repo.insert(&ok_b).await.unwrap();

        let outcome = repo.summary_outcome("2026-07", None, 0).await.unwrap();
        assert_eq!(outcome.request_count, 3);
        assert_eq!(outcome.success_count, 2);
        assert!((outcome.success_rate - 2.0 / 3.0).abs() < 1e-9);
        let avg_ttfb = outcome.avg_ttfb_ms.unwrap();
        assert!((avg_ttfb - 60.0).abs() < 1e-6); // (40+80)/2
        // ok: 1 tok / (100-40)ms; ok_b: 2 tok / (200-80)ms; err has 0 tokens, excluded.
        // sum/sum = 3 tok / 180ms * 1000 = 16.666.. tok/s
        let tps = outcome.tokens_per_sec.unwrap();
        assert!((tps - 16.666_666_666_666_668).abs() < 1e-6);

        let by_p = repo.summary_by_provider("2026-07", None, 0).await.unwrap();
        let a = by_p.iter().find(|p| p.provider_id == "prov-a").unwrap();
        assert_eq!(a.request_count, 2);
        assert!((a.success_rate - 0.5).abs() < 1e-9);
        assert!((a.avg_ttfb_ms.unwrap() - 40.0).abs() < 1e-6);
        // Only the ok row is eligible (err has 0 completion_tokens): 1 tok / 60ms * 1000.
        assert!((a.tokens_per_sec.unwrap() - 16.666_666_666_666_668).abs() < 1e-6);
        let b = by_p.iter().find(|p| p.provider_id == "prov-b").unwrap();
        assert_eq!(b.request_count, 1);
        assert!((b.success_rate - 1.0).abs() < 1e-9);
        // 2 tok / (200-80)ms * 1000.
        assert!((b.tokens_per_sec.unwrap() - 16.666_666_666_666_668).abs() < 1e-6);
    }

    #[tokio::test]
    async fn tokens_per_sec_edge_cases() {
        let pool = open_db("sqlite::memory:").await.unwrap();
        let repo = UsageRepo::new(&pool);

        // Row with no duration_ms at all must not leak its completion_tokens
        // into the numerator (numerator/denominator filters must match).
        let mut no_duration = new_usage_record(
            "r-no-duration",
            Some("k1".into()),
            None,
            Some("prov-a".into()),
            Some("openai".into()),
            Some("m".into()),
            5,
            10,
            15,
            0,
            0,
            0,
            0.05,
            true,
        );
        no_duration.ts = "2026-07-01T00:00:00Z".into();
        no_duration.status = "ok".into();
        no_duration.duration_ms = None;
        no_duration.ttfb_ms = None;
        repo.insert(&no_duration).await.unwrap();

        // Dirty data: ttfb_ms > duration_ms must clamp generation time to 0,
        // contributing 0 to the denominator (not negative).
        let mut dirty = new_usage_record(
            "r-dirty",
            Some("k1".into()),
            None,
            Some("prov-a".into()),
            Some("openai".into()),
            Some("m".into()),
            1,
            20,
            21,
            0,
            0,
            0,
            0.01,
            true,
        );
        dirty.ts = "2026-07-02T00:00:00Z".into();
        dirty.status = "ok".into();
        dirty.duration_ms = Some(100);
        dirty.ttfb_ms = Some(150);
        repo.insert(&dirty).await.unwrap();

        // A normal eligible row so the denominator isn't 0 overall -- this
        // makes the no_duration-leak scenario distinguishable: if its 10
        // tokens wrongly leaked into the numerator, the rate would be
        // (10+20+6)*1000/100 = 360 instead of the correct (20+6)*1000/100 = 260.
        let mut valid = new_usage_record(
            "r-valid",
            Some("k1".into()),
            None,
            Some("prov-a".into()),
            Some("openai".into()),
            Some("m".into()),
            3,
            6,
            9,
            0,
            0,
            0,
            0.02,
            true,
        );
        valid.ts = "2026-07-03T00:00:00Z".into();
        valid.status = "ok".into();
        valid.duration_ms = Some(100);
        valid.ttfb_ms = Some(0);
        repo.insert(&valid).await.unwrap();

        let outcome = repo.summary_outcome("2026-07", None, 0).await.unwrap();
        // no_duration is excluded entirely (duration_ms IS NULL); dirty
        // contributes 20 tokens / 0ms (clamped); valid contributes 6 tokens /
        // 100ms. sum/sum = (20+6)*1000/(0+100) = 260 tok/s.
        let tps = outcome.tokens_per_sec.unwrap();
        assert!((tps - 260.0).abs() < 1e-6);

        let by_p = repo.summary_by_provider("2026-07", None, 0).await.unwrap();
        let a = by_p.iter().find(|p| p.provider_id == "prov-a").unwrap();
        assert!((a.tokens_per_sec.unwrap() - 260.0).abs() < 1e-6);

        // A group where every row has completion_tokens == 0 must yield None,
        // not 0.0 or a panic from division by zero.
        let pool2 = open_db("sqlite::memory:").await.unwrap();
        let repo2 = UsageRepo::new(&pool2);
        let mut all_error = new_usage_record(
            "r-all-error",
            Some("k1".into()),
            None,
            Some("prov-z".into()),
            Some("openai".into()),
            Some("m".into()),
            5,
            0,
            5,
            0,
            0,
            0,
            0.0,
            false,
        );
        all_error.ts = "2026-07-03T00:00:00Z".into();
        all_error.status = "error".into();
        all_error.duration_ms = Some(10);
        repo2.insert(&all_error).await.unwrap();

        let outcome2 = repo2.summary_outcome("2026-07", None, 0).await.unwrap();
        assert!(outcome2.tokens_per_sec.is_none());

        let by_p2 = repo2.summary_by_provider("2026-07", None, 0).await.unwrap();
        assert_eq!(by_p2.len(), 1);
        assert!(by_p2[0].tokens_per_sec.is_none());
    }

    #[tokio::test]
    async fn summary_by_model_tokens_per_sec() {
        let pool = open_db("sqlite::memory:").await.unwrap();
        let repo = UsageRepo::new(&pool);

        let mut fast = new_usage_record(
            "r-fast",
            Some("k1".into()),
            Some("gpt-fast".into()),
            Some("prov-a".into()),
            Some("openai".into()),
            Some("gpt-fast".into()),
            1,
            100,
            101,
            0,
            0,
            0,
            0.1,
            true,
        );
        fast.ts = "2026-07-10T00:00:00Z".into();
        fast.status = "ok".into();
        fast.ttfb_ms = Some(0);
        fast.duration_ms = Some(500); // 100 tok / 500ms * 1000 = 200 tok/s
        repo.insert(&fast).await.unwrap();

        let mut slow = new_usage_record(
            "r-slow",
            Some("k1".into()),
            Some("gpt-slow".into()),
            Some("prov-a".into()),
            Some("openai".into()),
            Some("gpt-slow".into()),
            1,
            50,
            51,
            0,
            0,
            0,
            0.1,
            true,
        );
        slow.ts = "2026-07-10T00:00:00Z".into();
        slow.status = "ok".into();
        slow.ttfb_ms = Some(0);
        slow.duration_ms = Some(1000); // 50 tok / 1000ms * 1000 = 50 tok/s
        repo.insert(&slow).await.unwrap();

        let by_model = repo.summary_by_model("2026-07", None, 0).await.unwrap();
        let fast_row = by_model.iter().find(|m| m.label == "gpt-fast").unwrap();
        assert!((fast_row.tokens_per_sec.unwrap() - 200.0).abs() < 1e-6);
        let slow_row = by_model.iter().find(|m| m.label == "gpt-slow").unwrap();
        assert!((slow_row.tokens_per_sec.unwrap() - 50.0).abs() < 1e-6);
    }
}
