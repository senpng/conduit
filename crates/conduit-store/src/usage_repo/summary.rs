//! Period / day / model / provider summary aggregates.

use sqlx::Row;
use tracing::instrument;

use super::sql::{
    offset_modifier, period_day_like, LOCAL_DAY, PERIOD_KEY_WHERE, TOKENS_PER_SEC,
};
use super::types::{
    UsageDayModelRow, UsageDayRow, UsageKeyModelRow, UsageModelRow, UsageOutcomeSummary,
    UsageProviderRow, UsageSummaryRow,
};
use super::UsageRepo;
use crate::StoreError;

impl<'a> UsageRepo<'a> {
    /// Run a period-scoped aggregate query.
    ///
    /// Binds `(offset_modifier, period_day_like, key_id)` — the same order as
    /// [`PERIOD_KEY_WHERE`]. Returns raw rows for the caller to map.
    async fn query_period(
        &self,
        sql: &str,
        period: &str,
        key_id: Option<&str>,
        tz_offset_minutes: i32,
    ) -> Result<Vec<sqlx::sqlite::SqliteRow>, StoreError> {
        let off = offset_modifier(tz_offset_minutes);
        sqlx::query(sql)
            .bind(&off)
            .bind(period_day_like(period))
            .bind(key_id)
            .fetch_all(self.pool)
            .await
            .map_err(|e| StoreError::Sqlx(e.to_string()))
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
        let sql = format!(
            r#"SELECT
                   COALESCE(downstream_key_id, '') AS downstream_key_id,
                   COUNT(*) AS request_count,
                   COALESCE(SUM(cost_usd), 0) AS total_usd,
                   COALESCE(SUM(prompt_tokens), 0) AS prompt_tokens,
                   COALESCE(SUM(completion_tokens), 0) AS completion_tokens,
                   COALESCE(SUM(total_tokens), 0) AS total_tokens
               FROM usage_records
               {PERIOD_KEY_WHERE}
               GROUP BY COALESCE(downstream_key_id, '')
               ORDER BY total_usd DESC"#
        );
        let rows = self
            .query_period(&sql, period, None, tz_offset_minutes)
            .await?;
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
        // `?1` offset, `?2` since_day lower bound, `?3` optional key.
        let sql = format!(
            r#"SELECT
                   {LOCAL_DAY} AS day,
                   COUNT(*) AS request_count,
                   COALESCE(SUM(cost_usd), 0) AS total_usd,
                   COALESCE(SUM(total_tokens), 0) AS total_tokens
               FROM usage_records
               WHERE {LOCAL_DAY} >= ?2
                 AND (?3 IS NULL OR downstream_key_id = ?3)
               GROUP BY {LOCAL_DAY}
               ORDER BY day ASC"#
        );
        let rows = sqlx::query(&sql)
            .bind(&off)
            .bind(since_day)
            .bind(key_id)
            .fetch_all(self.pool)
            .await
            .map_err(|e| StoreError::Sqlx(e.to_string()))?;
        Ok(rows.into_iter().map(map_day_row).collect())
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
        let sql = format!(
            r#"SELECT
                   {LOCAL_DAY} AS day,
                   COUNT(*) AS request_count,
                   COALESCE(SUM(cost_usd), 0) AS total_usd,
                   COALESCE(SUM(total_tokens), 0) AS total_tokens
               FROM usage_records
               {PERIOD_KEY_WHERE}
               GROUP BY {LOCAL_DAY}
               ORDER BY day ASC"#
        );
        let rows = self
            .query_period(&sql, period, key_id, tz_offset_minutes)
            .await?;
        Ok(rows.into_iter().map(map_day_row).collect())
    }

    /// Model/alias rollup for a calendar period (`YYYY-MM`) or all-time (`"all"`).
    #[instrument(skip(self))]
    pub async fn summary_by_model(
        &self,
        period: &str,
        key_id: Option<&str>,
        tz_offset_minutes: i32,
    ) -> Result<Vec<UsageModelRow>, StoreError> {
        let sql = format!(
            r#"SELECT
                   COALESCE(NULLIF(alias, ''), NULLIF(model_id, ''), '(unknown)') AS label,
                   provider_kind,
                   COUNT(*) AS request_count,
                   COALESCE(SUM(cost_usd), 0) AS total_usd,
                   COALESCE(SUM(total_tokens), 0) AS total_tokens,
                   {TOKENS_PER_SEC}
               FROM usage_records
               {PERIOD_KEY_WHERE}
               GROUP BY COALESCE(NULLIF(alias, ''), NULLIF(model_id, ''), '(unknown)'),
                        provider_kind
               ORDER BY total_usd DESC"#
        );
        let rows = self
            .query_period(&sql, period, key_id, tz_offset_minutes)
            .await?;
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
        let sql = format!(
            r#"SELECT
                   COALESCE(downstream_key_id, '') AS downstream_key_id,
                   COALESCE(NULLIF(alias, ''), NULLIF(model_id, ''), '(unknown)') AS label,
                   provider_kind,
                   COUNT(*) AS request_count,
                   COALESCE(SUM(cost_usd), 0) AS total_usd,
                   COALESCE(SUM(total_tokens), 0) AS total_tokens
               FROM usage_records
               {PERIOD_KEY_WHERE}
               GROUP BY COALESCE(downstream_key_id, ''),
                        COALESCE(NULLIF(alias, ''), NULLIF(model_id, ''), '(unknown)'),
                        provider_kind
               ORDER BY total_usd DESC"#
        );
        let rows = self
            .query_period(&sql, period, None, tz_offset_minutes)
            .await?;
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
        let sql = format!(
            r#"SELECT
                   {LOCAL_DAY} AS day,
                   COALESCE(NULLIF(alias, ''), NULLIF(model_id, ''), '(unknown)') AS label,
                   provider_kind,
                   COUNT(*) AS request_count,
                   COALESCE(SUM(cost_usd), 0) AS total_usd,
                   COALESCE(SUM(total_tokens), 0) AS total_tokens
               FROM usage_records
               {PERIOD_KEY_WHERE}
               GROUP BY {LOCAL_DAY},
                        COALESCE(NULLIF(alias, ''), NULLIF(model_id, ''), '(unknown)'),
                        provider_kind
               ORDER BY day ASC, total_usd DESC"#
        );
        let rows = self
            .query_period(&sql, period, None, tz_offset_minutes)
            .await?;
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
        let sql = format!(
            r#"SELECT
                   COUNT(*) AS request_count,
                   COALESCE(SUM(CASE WHEN status = 'ok' THEN 1 ELSE 0 END), 0)
                       AS success_count,
                   AVG(ttfb_ms) AS avg_ttfb_ms,
                   AVG(duration_ms) AS avg_duration_ms,
                   {TOKENS_PER_SEC}
               FROM usage_records
               {PERIOD_KEY_WHERE}"#
        );
        let row = sqlx::query(&sql)
            .bind(&off)
            .bind(period_day_like(period))
            .bind(key_id)
            .fetch_one(self.pool)
            .await
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
        let sql = format!(
            r#"SELECT
                   COALESCE(NULLIF(provider_id, ''), '(unknown)') AS provider_id,
                   provider_kind,
                   COUNT(*) AS request_count,
                   COALESCE(SUM(CASE WHEN status = 'ok' THEN 1 ELSE 0 END), 0)
                       AS success_count,
                   AVG(ttfb_ms) AS avg_ttfb_ms,
                   AVG(duration_ms) AS avg_duration_ms,
                   {TOKENS_PER_SEC},
                   COALESCE(SUM(cost_usd), 0) AS total_usd,
                   COALESCE(SUM(total_tokens), 0) AS total_tokens
               FROM usage_records
               {PERIOD_KEY_WHERE}
               GROUP BY COALESCE(NULLIF(provider_id, ''), '(unknown)'), provider_kind
               ORDER BY total_tokens DESC"#
        );
        let rows = self
            .query_period(&sql, period, key_id, tz_offset_minutes)
            .await?;
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

fn map_day_row(r: sqlx::sqlite::SqliteRow) -> UsageDayRow {
    UsageDayRow {
        day: r.get("day"),
        request_count: r.get::<i64, _>("request_count") as u64,
        total_usd: r.get("total_usd"),
        total_tokens: r.get::<i64, _>("total_tokens") as u64,
    }
}
