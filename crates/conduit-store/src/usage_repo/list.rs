//! Paginated usage list queries.

use tracing::instrument;

use super::map::map_row;
use super::sql::offset_modifier;
use super::types::{UsageListOpts, UsageListPage, UsageListSort};
use super::UsageRepo;
use crate::{schema::UsageRecordRow, StoreError};

impl<'a> UsageRepo<'a> {
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
                   pool_id, selected_reason, loss_count, wire_format
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

}
