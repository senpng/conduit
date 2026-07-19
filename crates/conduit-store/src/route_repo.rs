use chrono::Utc;
use sqlx::{Row, SqlitePool};
use tracing::instrument;

use crate::{schema::RouteRow, StoreError};

pub struct RouteRepo<'a> {
    pool: &'a SqlitePool,
}

impl<'a> RouteRepo<'a> {
    pub fn new(pool: &'a SqlitePool) -> Self {
        Self { pool }
    }

    #[instrument(skip(self, row))]
    pub async fn upsert(&self, row: &RouteRow) -> Result<(), StoreError> {
        sqlx::query(
            r#"INSERT INTO routes
               (id, match_alias, strategy, targets_json, retry_policy_json, enabled, created_at, updated_at, deleted_at)
               VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)
               ON CONFLICT(id) DO UPDATE SET
                   match_alias        = excluded.match_alias,
                   strategy           = excluded.strategy,
                   targets_json       = excluded.targets_json,
                   retry_policy_json  = excluded.retry_policy_json,
                   enabled            = excluded.enabled,
                   updated_at         = excluded.updated_at,
                   deleted_at         = excluded.deleted_at"#,
        )
        .bind(&row.id)
        .bind(&row.match_alias)
        .bind(&row.strategy)
        .bind(&row.targets_json)
        .bind(&row.retry_policy_json)
        .bind(row.enabled as i32)
        .bind(&row.created_at)
        .bind(&row.updated_at)
        .bind(&row.deleted_at)
        .execute(self.pool)
        .await
        .map_err(|e| StoreError::Sqlx(e.to_string()))?;
        Ok(())
    }

    #[instrument(skip(self, row))]
    pub async fn insert(&self, row: &RouteRow) -> Result<(), StoreError> {
        sqlx::query(
            r#"INSERT INTO routes
               (id, match_alias, strategy, targets_json, retry_policy_json, enabled, created_at, updated_at, deleted_at)
               VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)"#,
        )
        .bind(&row.id)
        .bind(&row.match_alias)
        .bind(&row.strategy)
        .bind(&row.targets_json)
        .bind(&row.retry_policy_json)
        .bind(row.enabled as i32)
        .bind(&row.created_at)
        .bind(&row.updated_at)
        .bind(&row.deleted_at)
        .execute(self.pool)
        .await
        .map_err(|e| StoreError::Sqlx(e.to_string()))?;
        Ok(())
    }

    /// Active (non-deleted) route by id.
    #[instrument(skip(self))]
    pub async fn get(&self, id: &str) -> Result<Option<RouteRow>, StoreError> {
        let row = sqlx::query(
            "SELECT id, match_alias, strategy, targets_json, retry_policy_json, enabled, created_at, updated_at, deleted_at
             FROM routes WHERE id = ? AND deleted_at IS NULL",
        )
        .bind(id)
        .fetch_optional(self.pool)
        .await
        .map_err(|e| StoreError::Sqlx(e.to_string()))?
        .map(map_route_row);
        Ok(row)
    }

    #[instrument(skip(self))]
    pub async fn get_by_alias(&self, alias: &str) -> Result<Option<RouteRow>, StoreError> {
        let row = sqlx::query(
            "SELECT id, match_alias, strategy, targets_json, retry_policy_json, enabled, created_at, updated_at, deleted_at
             FROM routes WHERE match_alias = ? AND enabled = 1 AND deleted_at IS NULL",
        )
        .bind(alias)
        .fetch_optional(self.pool)
        .await
        .map_err(|e| StoreError::Sqlx(e.to_string()))?
        .map(map_route_row);
        Ok(row)
    }

    /// List enabled, non-deleted routes (gateway / routing table).
    #[instrument(skip(self))]
    pub async fn list(&self) -> Result<Vec<RouteRow>, StoreError> {
        let rows = sqlx::query(
            "SELECT id, match_alias, strategy, targets_json, retry_policy_json, enabled, created_at, updated_at, deleted_at
             FROM routes WHERE enabled = 1 AND deleted_at IS NULL ORDER BY match_alias ASC",
        )
        .fetch_all(self.pool)
        .await
        .map_err(|e| StoreError::Sqlx(e.to_string()))?
        .into_iter()
        .map(map_route_row)
        .collect();
        Ok(rows)
    }

    /// List non-deleted routes including disabled ones (console).
    #[instrument(skip(self))]
    pub async fn list_all(&self) -> Result<Vec<RouteRow>, StoreError> {
        let rows = sqlx::query(
            "SELECT id, match_alias, strategy, targets_json, retry_policy_json, enabled, created_at, updated_at, deleted_at
             FROM routes WHERE deleted_at IS NULL ORDER BY match_alias ASC",
        )
        .fetch_all(self.pool)
        .await
        .map_err(|e| StoreError::Sqlx(e.to_string()))?
        .into_iter()
        .map(map_route_row)
        .collect();
        Ok(rows)
    }

    #[instrument(skip(self))]
    pub async fn set_enabled(&self, id: &str, enabled: bool) -> Result<(), StoreError> {
        let now = Utc::now().to_rfc3339();
        sqlx::query(
            "UPDATE routes SET enabled = ?, updated_at = ?
             WHERE id = ? AND deleted_at IS NULL",
        )
        .bind(enabled as i32)
        .bind(&now)
        .bind(id)
        .execute(self.pool)
        .await
        .map_err(|e| StoreError::Sqlx(e.to_string()))?;
        Ok(())
    }

    /// Soft-delete: set `deleted_at`. Frees `match_alias` for reuse.
    #[instrument(skip(self))]
    pub async fn delete(&self, id: &str) -> Result<(), StoreError> {
        let now = Utc::now().to_rfc3339();
        sqlx::query(
            "UPDATE routes SET deleted_at = ?, updated_at = ?
             WHERE id = ? AND deleted_at IS NULL",
        )
        .bind(&now)
        .bind(&now)
        .bind(id)
        .execute(self.pool)
        .await
        .map_err(|e| StoreError::Sqlx(e.to_string()))?;
        Ok(())
    }
}

fn map_route_row(r: sqlx::sqlite::SqliteRow) -> RouteRow {
    RouteRow {
        id: r.get("id"),
        match_alias: r.get("match_alias"),
        strategy: r.get("strategy"),
        targets_json: r.get("targets_json"),
        retry_policy_json: r
            .get::<Option<String>, _>("retry_policy_json")
            .unwrap_or_else(|| "{}".to_string()),
        enabled: r.get::<i32, _>("enabled") != 0,
        created_at: r.get("created_at"),
        updated_at: r.get("updated_at"),
        deleted_at: r.get("deleted_at"),
    }
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use chrono::Utc;

    use super::*;
    use crate::open_db;

    fn make_row(id: &str, alias: &str) -> RouteRow {
        let now = Utc::now().to_rfc3339();
        RouteRow {
            id: id.to_string(),
            match_alias: alias.to_string(),
            strategy: "fixed".into(),
            targets_json: r#"[{"provider_id":"openai","model_id":"gpt-4o","provider_kind":"openai"}]"#.into(),
            retry_policy_json: r#"{"max_retries":2,"base_delay_ms":500,"retryable_statuses":[429,500]}"#.into(),
            enabled: true,
            created_at: now.clone(),
            updated_at: now,
            deleted_at: None,
        }
    }

    #[tokio::test]
    async fn insert_get_soft_delete() {
        let pool = open_db("sqlite::memory:").await.unwrap();
        let repo = RouteRepo::new(&pool);

        repo.insert(&make_row("r1", "gpt-4o")).await.unwrap();
        let got = repo.get("r1").await.unwrap().unwrap();
        assert_eq!(got.match_alias, "gpt-4o");

        repo.delete("r1").await.unwrap();
        assert!(repo.get("r1").await.unwrap().is_none());
        assert!(repo.list().await.unwrap().is_empty());
        assert!(repo.list_all().await.unwrap().is_empty());

        let raw: Option<(String,)> =
            sqlx::query_as("SELECT id FROM routes WHERE id = 'r1'")
                .fetch_optional(&pool)
                .await
                .unwrap();
        assert!(raw.is_some());
    }

    #[tokio::test]
    async fn soft_delete_frees_match_alias() {
        let pool = open_db("sqlite::memory:").await.unwrap();
        let repo = RouteRepo::new(&pool);

        repo.insert(&make_row("r1", "fast")).await.unwrap();
        repo.delete("r1").await.unwrap();
        // Same alias can be recreated under a new id.
        repo.insert(&make_row("r2", "fast")).await.unwrap();
        let got = repo.get_by_alias("fast").await.unwrap().unwrap();
        assert_eq!(got.id, "r2");
    }

    #[tokio::test]
    async fn upsert_updates_fields() {
        let pool = open_db("sqlite::memory:").await.unwrap();
        let repo = RouteRepo::new(&pool);

        repo.upsert(&make_row("r2", "alias-a")).await.unwrap();

        let mut updated = make_row("r2", "alias-a");
        updated.strategy = "fallback".into();
        repo.upsert(&updated).await.unwrap();

        let got = repo.get("r2").await.unwrap().unwrap();
        assert_eq!(got.strategy, "fallback");
    }

    #[tokio::test]
    async fn retry_policy_json_survives_round_trip() {
        let pool = open_db("sqlite::memory:").await.unwrap();
        let repo = RouteRepo::new(&pool);

        let row = make_row("r3", "fast");
        repo.upsert(&row).await.unwrap();

        let got = repo.get("r3").await.unwrap().unwrap();
        let rp: serde_json::Value = serde_json::from_str(&got.retry_policy_json).unwrap();
        assert_eq!(rp["max_retries"], 2);
        assert_eq!(rp["base_delay_ms"], 500);
    }

    #[tokio::test]
    async fn list_excludes_disabled_and_deleted() {
        let pool = open_db("sqlite::memory:").await.unwrap();
        let repo = RouteRepo::new(&pool);

        repo.insert(&make_row("r4", "visible")).await.unwrap();

        let mut hidden = make_row("r5", "hidden");
        hidden.enabled = false;
        repo.insert(&hidden).await.unwrap();

        repo.insert(&make_row("r6", "gone")).await.unwrap();
        repo.delete("r6").await.unwrap();

        let rows = repo.list().await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].match_alias, "visible");

        let all = repo.list_all().await.unwrap();
        assert_eq!(all.len(), 2); // visible + disabled, not deleted
    }
}
