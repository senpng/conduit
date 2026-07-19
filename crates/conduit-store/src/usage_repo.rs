//! Per-request usage ledger.
//!
//! Every completed request with non-zero tokens or cost is written here.

use chrono::Utc;
use sqlx::{Row, SqlitePool};
use tracing::instrument;

use crate::{schema::UsageRecordRow, StoreError};

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
            })
            .await?;
        Ok(page.rows)
    }

    /// Paginated usage list with optional key / period / free-text filter and sort.
    ///
    /// - `period` (`YYYY-MM`) scopes rows with `ts LIKE 'YYYY-MM%'`.
    /// - `key_id` scopes to one downstream key.
    /// - `q` matches (case-insensitive substring) model, alias, provider, request id, key id.
    /// - `sort` is always descending (date / cost / tokens), with `ts` as tie-breaker.
    #[instrument(skip(self))]
    pub async fn list_page(&self, opts: UsageListOpts<'_>) -> Result<UsageListPage, StoreError> {
        let limit = opts.limit.clamp(1, 500) as i64;
        let offset = opts.offset as i64;
        let key_id = opts.key_id.map(str::trim).filter(|s| !s.is_empty());
        let period_pat = opts
            .period
            .map(str::trim)
            .filter(|p| !p.is_empty())
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

        // Optional filters: bind NULL / empty to skip the predicate.
        let count_sql = r#"
            SELECT COUNT(*) AS cnt
            FROM usage_records
            WHERE (?1 IS NULL OR downstream_key_id = ?1)
              AND (?2 IS NULL OR ts LIKE ?2)
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
            .fetch_one(self.pool)
            .await
            .map_err(|e| StoreError::Sqlx(e.to_string()))?;

        let list_sql = format!(
            r#"
            SELECT id, ts, request_id, downstream_key_id, alias,
                   provider_id, provider_kind, model_id,
                   prompt_tokens, completion_tokens, total_tokens,
                   reasoning_tokens, cache_read_tokens, cache_write_tokens,
                   cost_usd, stream
            FROM usage_records
            WHERE (?1 IS NULL OR downstream_key_id = ?1)
              AND (?2 IS NULL OR ts LIKE ?2)
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
            LIMIT ?4 OFFSET ?5
            "#
        );
        let rows = sqlx::query(&list_sql)
            .bind(key_id)
            .bind(period_pat.as_deref())
            .bind(&q)
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

    /// Aggregate spend by downstream key for a calendar period (`YYYY-MM`).
    ///
    /// Period is matched on the ISO timestamp prefix (`ts LIKE 'YYYY-MM%'`).
    #[instrument(skip(self))]
    pub async fn summary_period(&self, period: &str) -> Result<Vec<UsageSummaryRow>, StoreError> {
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
            Some(kid) => {
                sqlx::query(
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
                .await
            }
            None => {
                sqlx::query(
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

    /// Model/alias rollup for a calendar period (`YYYY-MM`).
    #[instrument(skip(self))]
    pub async fn summary_by_model(
        &self,
        period: &str,
        key_id: Option<&str>,
    ) -> Result<Vec<UsageModelRow>, StoreError> {
        let pattern = format!("{period}%");
        let rows = match key_id {
            Some(kid) => {
                sqlx::query(
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
                .await
            }
            None => {
                sqlx::query(
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
            })
            .collect())
    }

    /// Model breakdown nested under each downstream key for a calendar period.
    #[instrument(skip(self))]
    pub async fn summary_by_key_model(
        &self,
        period: &str,
    ) -> Result<Vec<UsageKeyModelRow>, StoreError> {
        let pattern = format!("{period}%");
        let rows = sqlx::query(
            r#"SELECT
                   COALESCE(downstream_key_id, '') AS downstream_key_id,
                   COALESCE(NULLIF(alias, ''), NULLIF(model_id, ''), '(unknown)') AS label,
                   provider_kind,
                   COUNT(*) AS request_count,
                   COALESCE(SUM(cost_usd), 0) AS total_usd,
                   COALESCE(SUM(total_tokens), 0) AS total_tokens
               FROM usage_records
               WHERE ts LIKE ?
               GROUP BY COALESCE(downstream_key_id, ''),
                        COALESCE(NULLIF(alias, ''), NULLIF(model_id, ''), '(unknown)'),
                        provider_kind
               ORDER BY total_usd DESC"#,
        )
        .bind(&pattern)
        .fetch_all(self.pool)
        .await
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

    /// Model breakdown nested under each UTC day for a calendar period.
    #[instrument(skip(self))]
    pub async fn summary_by_day_model(
        &self,
        period: &str,
    ) -> Result<Vec<UsageDayModelRow>, StoreError> {
        let pattern = format!("{period}%");
        let rows = sqlx::query(
            r#"SELECT
                   substr(ts, 1, 10) AS day,
                   COALESCE(NULLIF(alias, ''), NULLIF(model_id, ''), '(unknown)') AS label,
                   provider_kind,
                   COUNT(*) AS request_count,
                   COALESCE(SUM(cost_usd), 0) AS total_usd,
                   COALESCE(SUM(total_tokens), 0) AS total_tokens
               FROM usage_records
               WHERE ts LIKE ?
               GROUP BY substr(ts, 1, 10),
                        COALESCE(NULLIF(alias, ''), NULLIF(model_id, ''), '(unknown)'),
                        provider_kind
               ORDER BY day ASC, total_usd DESC"#,
        )
        .bind(&pattern)
        .fetch_all(self.pool)
        .await
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
