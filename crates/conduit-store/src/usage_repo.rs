//! Per-request usage ledger.
//!
//! Independent of the trace log: every completed request with non-zero usage
//! or cost is written here so spend remains queryable when traces are disabled.

use chrono::Utc;
use sqlx::{Row, SqlitePool};
use tracing::instrument;

use crate::{schema::UsageRecordRow, StoreError};

pub struct UsageRepo<'a> {
    pool: &'a SqlitePool,
}

impl<'a> UsageRepo<'a> {
    pub fn new(pool: &'a SqlitePool) -> Self {
        Self { pool }
    }

    /// Insert one request consumption row.
    #[instrument(skip(self, row))]
    pub async fn insert(&self, row: &UsageRecordRow) -> Result<(), StoreError> {
        sqlx::query(
            r#"INSERT INTO usage_records (
                   id, ts, request_id, downstream_key_id, alias,
                   provider_id, provider_kind, model_id,
                   prompt_tokens, completion_tokens, total_tokens,
                   reasoning_tokens, cache_read_tokens, cache_write_tokens,
                   cost_usd, stream
               ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)"#,
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
        .execute(self.pool)
        .await
        .map_err(|e| StoreError::Sqlx(e.to_string()))?;
        Ok(())
    }

    /// Recent usage rows, newest first.
    #[instrument(skip(self))]
    pub async fn list(
        &self,
        limit: usize,
        key_id: Option<&str>,
    ) -> Result<Vec<UsageRecordRow>, StoreError> {
        let limit = limit.clamp(1, 500) as i64;
        let rows = match key_id {
            Some(kid) => sqlx::query(
                r#"SELECT id, ts, request_id, downstream_key_id, alias,
                          provider_id, provider_kind, model_id,
                          prompt_tokens, completion_tokens, total_tokens,
                          reasoning_tokens, cache_read_tokens, cache_write_tokens,
                          cost_usd, stream
                   FROM usage_records
                   WHERE downstream_key_id = ?
                   ORDER BY ts DESC
                   LIMIT ?"#,
            )
            .bind(kid)
            .bind(limit)
            .fetch_all(self.pool)
            .await,
            None => sqlx::query(
                r#"SELECT id, ts, request_id, downstream_key_id, alias,
                          provider_id, provider_kind, model_id,
                          prompt_tokens, completion_tokens, total_tokens,
                          reasoning_tokens, cache_read_tokens, cache_write_tokens,
                          cost_usd, stream
                   FROM usage_records
                   ORDER BY ts DESC
                   LIMIT ?"#,
            )
            .bind(limit)
            .fetch_all(self.pool)
            .await,
        }
        .map_err(|e| StoreError::Sqlx(e.to_string()))?;

        Ok(rows.into_iter().map(map_row).collect())
    }

    /// Aggregate spend by downstream key for a calendar period (`YYYY-MM`).
    ///
    /// Period is matched on the ISO timestamp prefix (`ts LIKE 'YYYY-MM%'`).
    #[instrument(skip(self))]
    pub async fn summary_period(
        &self,
        period: &str,
    ) -> Result<Vec<UsageSummaryRow>, StoreError> {
        let pattern = format!("{period}%");
        let rows = sqlx::query(
            r#"SELECT
                   COALESCE(downstream_key_id, '') AS downstream_key_id,
                   COUNT(*) AS request_count,
                   COALESCE(SUM(cost_usd), 0) AS total_usd,
                   COALESCE(SUM(prompt_tokens), 0) AS prompt_tokens,
                   COALESCE(SUM(completion_tokens), 0) AS completion_tokens,
                   COALESCE(SUM(total_tokens), 0) AS total_tokens
               FROM usage_records
               WHERE ts LIKE ?
               GROUP BY COALESCE(downstream_key_id, '')
               ORDER BY total_usd DESC"#,
        )
        .bind(&pattern)
        .fetch_all(self.pool)
        .await
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

    /// Daily rollup for a calendar period (`YYYY-MM`), UTC day from `ts` prefix.
    ///
    /// Optional `key_id` scopes to one downstream key. Used by the Usage UI so
    /// "Daily spend" is period-accurate (not limited to the recent-N records window).
    #[instrument(skip(self))]
    pub async fn summary_by_day(
        &self,
        period: &str,
        key_id: Option<&str>,
    ) -> Result<Vec<UsageDayRow>, StoreError> {
        let pattern = format!("{period}%");
        let rows = match key_id {
            Some(kid) => sqlx::query(
                r#"SELECT
                       substr(ts, 1, 10) AS day,
                       COUNT(*) AS request_count,
                       COALESCE(SUM(cost_usd), 0) AS total_usd,
                       COALESCE(SUM(total_tokens), 0) AS total_tokens
                   FROM usage_records
                   WHERE ts LIKE ? AND downstream_key_id = ?
                   GROUP BY substr(ts, 1, 10)
                   ORDER BY day ASC"#,
            )
            .bind(&pattern)
            .bind(kid)
            .fetch_all(self.pool)
            .await,
            None => sqlx::query(
                r#"SELECT
                       substr(ts, 1, 10) AS day,
                       COUNT(*) AS request_count,
                       COALESCE(SUM(cost_usd), 0) AS total_usd,
                       COALESCE(SUM(total_tokens), 0) AS total_tokens
                   FROM usage_records
                   WHERE ts LIKE ?
                   GROUP BY substr(ts, 1, 10)
                   ORDER BY day ASC"#,
            )
            .bind(&pattern)
            .fetch_all(self.pool)
            .await,
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

    /// Model/alias rollup for a calendar period (`YYYY-MM`).
    #[instrument(skip(self))]
    pub async fn summary_by_model(
        &self,
        period: &str,
        key_id: Option<&str>,
    ) -> Result<Vec<UsageModelRow>, StoreError> {
        let pattern = format!("{period}%");
        let rows = match key_id {
            Some(kid) => sqlx::query(
                r#"SELECT
                       COALESCE(NULLIF(alias, ''), NULLIF(model_id, ''), '(unknown)') AS label,
                       provider_kind,
                       COUNT(*) AS request_count,
                       COALESCE(SUM(cost_usd), 0) AS total_usd,
                       COALESCE(SUM(total_tokens), 0) AS total_tokens
                   FROM usage_records
                   WHERE ts LIKE ? AND downstream_key_id = ?
                   GROUP BY COALESCE(NULLIF(alias, ''), NULLIF(model_id, ''), '(unknown)'),
                            provider_kind
                   ORDER BY total_usd DESC"#,
            )
            .bind(&pattern)
            .bind(kid)
            .fetch_all(self.pool)
            .await,
            None => sqlx::query(
                r#"SELECT
                       COALESCE(NULLIF(alias, ''), NULLIF(model_id, ''), '(unknown)') AS label,
                       provider_kind,
                       COUNT(*) AS request_count,
                       COALESCE(SUM(cost_usd), 0) AS total_usd,
                       COALESCE(SUM(total_tokens), 0) AS total_tokens
                   FROM usage_records
                   WHERE ts LIKE ?
                   GROUP BY COALESCE(NULLIF(alias, ''), NULLIF(model_id, ''), '(unknown)'),
                            provider_kind
                   ORDER BY total_usd DESC"#,
            )
            .bind(&pattern)
            .fetch_all(self.pool)
            .await,
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
            })
            .collect())
    }
}

/// Period rollup used by admin summary.
#[derive(Debug, Clone, PartialEq)]
pub struct UsageSummaryRow {
    pub downstream_key_id: String,
    pub request_count: u64,
    pub total_usd: f64,
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub total_tokens: u64,
}

/// One UTC calendar day within a period summary.
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
    }
}

/// Build a row for a completed request. Generates `id` and `ts`.
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

        let listed = repo.list(10, None).await.unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].request_id, "req-1");
        assert!((listed[0].cost_usd - 0.012).abs() < 1e-9);

        let by_key = repo.list(10, Some("dk1")).await.unwrap();
        assert_eq!(by_key.len(), 1);
        let empty = repo.list(10, Some("other")).await.unwrap();
        assert!(empty.is_empty());
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

        let sum = repo.summary_period("2026-07").await.unwrap();
        assert_eq!(sum.len(), 2);
        let k1 = sum.iter().find(|s| s.downstream_key_id == "k1").unwrap();
        assert_eq!(k1.request_count, 2);
        assert!((k1.total_usd - 3.5).abs() < 1e-9);
        assert_eq!(k1.total_tokens, 6);
        let k2 = sum.iter().find(|s| s.downstream_key_id == "k2").unwrap();
        assert_eq!(k2.request_count, 1);
        assert!((k2.total_usd - 0.5).abs() < 1e-9);

        let by_day = repo.summary_by_day("2026-07", None).await.unwrap();
        assert_eq!(by_day.len(), 3); // 01, 15, 20
        assert_eq!(by_day[0].day, "2026-07-01");
        assert!((by_day[0].total_usd - 1.0).abs() < 1e-9);
        let day15 = by_day.iter().find(|d| d.day == "2026-07-15").unwrap();
        assert!((day15.total_usd - 2.5).abs() < 1e-9);

        let k1_days = repo.summary_by_day("2026-07", Some("k1")).await.unwrap();
        assert_eq!(k1_days.len(), 2);
        assert!(k1_days.iter().all(|d| d.day.starts_with("2026-07")));

        // Alias rollup (rows above have no alias — label falls back to "(unknown)")
        let by_model = repo.summary_by_model("2026-07", None).await.unwrap();
        assert!(!by_model.is_empty());
        let unknown = by_model.iter().find(|m| m.label == "(unknown)").unwrap();
        assert_eq!(unknown.request_count, 3);
        assert!((unknown.total_usd - 4.0).abs() < 1e-9);
    }
}
