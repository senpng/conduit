use chrono::Utc;
use sqlx::{Row, SqlitePool};
use tracing::instrument;

use crate::{schema::DownstreamKeyRow, StoreError};

pub struct KeyRepo<'a> {
    pool: &'a SqlitePool,
}

impl<'a> KeyRepo<'a> {
    pub fn new(pool: &'a SqlitePool) -> Self {
        Self { pool }
    }

    #[instrument(skip(self, row))]
    pub async fn insert(&self, row: &DownstreamKeyRow) -> Result<(), StoreError> {
        sqlx::query(
            r#"INSERT INTO downstream_keys
               (id, name, key_hash, model_whitelist, monthly_budget_usd, rate_limit_rpm, enabled, created_at, updated_at, deleted_at)
               VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)"#,
        )
        .bind(&row.id)
        .bind(&row.name)
        .bind(&row.key_hash)
        .bind(&row.model_whitelist)
        .bind(row.monthly_budget_usd)
        .bind(row.rate_limit_rpm)
        .bind(row.enabled as i32)
        .bind(&row.created_at)
        .bind(&row.updated_at)
        .bind(&row.deleted_at)
        .execute(self.pool)
        .await
        .map_err(|e| StoreError::Sqlx(e.to_string()))?;
        Ok(())
    }

    /// Active (non-deleted) key by id.
    #[instrument(skip(self))]
    pub async fn get(&self, id: &str) -> Result<Option<DownstreamKeyRow>, StoreError> {
        let row = sqlx::query(
            "SELECT id, name, key_hash, model_whitelist, monthly_budget_usd, rate_limit_rpm, enabled, created_at, updated_at, deleted_at
             FROM downstream_keys WHERE id = ? AND deleted_at IS NULL",
        )
        .bind(id)
        .fetch_optional(self.pool)
        .await
        .map_err(|e| StoreError::Sqlx(e.to_string()))?
        .map(map_key_row);
        Ok(row)
    }

    /// Look up an enabled, non-deleted key by its BLAKE3 hash for authentication.
    #[instrument(skip(self))]
    pub async fn get_by_hash(&self, hash: &str) -> Result<Option<DownstreamKeyRow>, StoreError> {
        let row = sqlx::query(
            "SELECT id, name, key_hash, model_whitelist, monthly_budget_usd, rate_limit_rpm, enabled, created_at, updated_at, deleted_at
             FROM downstream_keys WHERE key_hash = ? AND enabled = 1 AND deleted_at IS NULL",
        )
        .bind(hash)
        .fetch_optional(self.pool)
        .await
        .map_err(|e| StoreError::Sqlx(e.to_string()))?
        .map(map_key_row);
        Ok(row)
    }

    /// Look up a key by hash ignoring enabled/deleted filters (auth diagnostics only).
    #[instrument(skip(self))]
    pub async fn get_by_hash_any(&self, hash: &str) -> Result<Option<DownstreamKeyRow>, StoreError> {
        let row = sqlx::query(
            "SELECT id, name, key_hash, model_whitelist, monthly_budget_usd, rate_limit_rpm, enabled, created_at, updated_at, deleted_at
             FROM downstream_keys WHERE key_hash = ?",
        )
        .bind(hash)
        .fetch_optional(self.pool)
        .await
        .map_err(|e| StoreError::Sqlx(e.to_string()))?
        .map(map_key_row);
        Ok(row)
    }

    /// Active keys only (includes disabled for console toggle).
    #[instrument(skip(self))]
    pub async fn list(&self) -> Result<Vec<DownstreamKeyRow>, StoreError> {
        let rows = sqlx::query(
            "SELECT id, name, key_hash, model_whitelist, monthly_budget_usd, rate_limit_rpm, enabled, created_at, updated_at, deleted_at
             FROM downstream_keys WHERE deleted_at IS NULL ORDER BY created_at DESC",
        )
        .fetch_all(self.pool)
        .await
        .map_err(|e| StoreError::Sqlx(e.to_string()))?
        .into_iter()
        .map(map_key_row)
        .collect();
        Ok(rows)
    }

    #[instrument(skip(self))]
    pub async fn set_enabled(&self, id: &str, enabled: bool) -> Result<(), StoreError> {
        let now = Utc::now().to_rfc3339();
        sqlx::query(
            "UPDATE downstream_keys SET enabled = ?, updated_at = ?
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

    /// Update rate-limit RPM for a key (`None` clears the limit).
    #[instrument(skip(self))]
    pub async fn set_rate_limit_rpm(
        &self,
        id: &str,
        rate_limit_rpm: Option<i64>,
    ) -> Result<(), StoreError> {
        let now = Utc::now().to_rfc3339();
        sqlx::query(
            "UPDATE downstream_keys SET rate_limit_rpm = ?, updated_at = ?
             WHERE id = ? AND deleted_at IS NULL",
        )
        .bind(rate_limit_rpm)
        .bind(&now)
        .bind(id)
        .execute(self.pool)
        .await
        .map_err(|e| StoreError::Sqlx(e.to_string()))?;
        Ok(())
    }

    /// Persist all administrator-editable fields in one atomic update.
    #[instrument(skip(self, row))]
    pub async fn update(&self, row: &DownstreamKeyRow) -> Result<(), StoreError> {
        sqlx::query(
            "UPDATE downstream_keys
             SET name = ?, model_whitelist = ?, rate_limit_rpm = ?, enabled = ?, updated_at = ?
             WHERE id = ? AND deleted_at IS NULL",
        )
        .bind(&row.name)
        .bind(&row.model_whitelist)
        .bind(row.rate_limit_rpm)
        .bind(row.enabled as i32)
        .bind(&row.updated_at)
        .bind(&row.id)
        .execute(self.pool)
        .await
        .map_err(|e| StoreError::Sqlx(e.to_string()))?;
        Ok(())
    }

    /// Soft-delete: set `deleted_at`. Auth and console list ignore the key.
    #[instrument(skip(self))]
    pub async fn delete(&self, id: &str) -> Result<(), StoreError> {
        let now = Utc::now().to_rfc3339();
        sqlx::query(
            "UPDATE downstream_keys SET deleted_at = ?, updated_at = ?
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

fn map_key_row(r: sqlx::sqlite::SqliteRow) -> DownstreamKeyRow {
    DownstreamKeyRow {
        id: r.get("id"),
        name: r.get("name"),
        key_hash: r.get("key_hash"),
        model_whitelist: r
            .get::<Option<String>, _>("model_whitelist")
            .unwrap_or_else(|| "[]".to_string()),
        monthly_budget_usd: r.get("monthly_budget_usd"),
        rate_limit_rpm: r.get("rate_limit_rpm"),
        enabled: r.get::<i32, _>("enabled") != 0,
        created_at: r.get("created_at"),
        updated_at: r.get("updated_at"),
        deleted_at: r.get("deleted_at"),
    }
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::open_db;

    fn make_key(id: &str, hash: &str) -> DownstreamKeyRow {
        let now = Utc::now().to_rfc3339();
        DownstreamKeyRow {
            id: id.to_string(),
            name: "Test Key".into(),
            key_hash: hash.to_string(),
            model_whitelist: "[]".into(),
            monthly_budget_usd: Some(10.0),
            rate_limit_rpm: Some(60),
            enabled: true,
            created_at: now.clone(),
            updated_at: now,
            deleted_at: None,
        }
    }

    #[tokio::test]
    async fn insert_get_by_hash() {
        let pool = open_db("sqlite::memory:").await.unwrap();
        let repo = KeyRepo::new(&pool);

        repo.insert(&make_key("k1", "abc123")).await.unwrap();
        let got = repo.get_by_hash("abc123").await.unwrap().unwrap();
        assert_eq!(got.id, "k1");
        assert_eq!(got.monthly_budget_usd, Some(10.0));
    }

    #[tokio::test]
    async fn disabled_key_not_found_by_hash() {
        let pool = open_db("sqlite::memory:").await.unwrap();
        let repo = KeyRepo::new(&pool);

        repo.insert(&make_key("k2", "def456")).await.unwrap();
        repo.set_enabled("k2", false).await.unwrap();

        let got = repo.get_by_hash("def456").await.unwrap();
        assert!(got.is_none());
        // Diagnostic path still sees the disabled row.
        let any = repo.get_by_hash_any("def456").await.unwrap().unwrap();
        assert!(!any.enabled);
        assert_eq!(any.id, "k2");
    }

    #[tokio::test]
    async fn soft_delete_hides_key() {
        let pool = open_db("sqlite::memory:").await.unwrap();
        let repo = KeyRepo::new(&pool);

        repo.insert(&make_key("k3", "ghi789")).await.unwrap();
        repo.delete("k3").await.unwrap();
        assert!(repo.get("k3").await.unwrap().is_none());
        assert!(repo.get_by_hash("ghi789").await.unwrap().is_none());
        assert!(repo.list().await.unwrap().is_empty());

        let raw: Option<(String,)> =
            sqlx::query_as("SELECT id FROM downstream_keys WHERE id = 'k3'")
                .fetch_optional(&pool)
                .await
                .unwrap();
        assert!(raw.is_some());
    }

    #[tokio::test]
    async fn update_persists_all_admin_editable_fields() {
        let pool = open_db("sqlite::memory:").await.unwrap();
        let repo = KeyRepo::new(&pool);
        let mut key = make_key("k4", "jkl012");
        repo.insert(&key).await.unwrap();

        key.name = "Renamed Key".into();
        key.model_whitelist = r#"[\"gpt-4o-mini\"]"#.into();
        key.rate_limit_rpm = Some(12);
        key.enabled = false;
        key.updated_at = "2026-07-18T00:00:00Z".into();
        repo.update(&key).await.unwrap();

        let stored = repo.get("k4").await.unwrap().unwrap();
        assert_eq!(stored.name, "Renamed Key");
        assert_eq!(stored.model_whitelist, r#"[\"gpt-4o-mini\"]"#);
        assert_eq!(stored.rate_limit_rpm, Some(12));
        assert!(!stored.enabled);
        assert_eq!(stored.updated_at, "2026-07-18T00:00:00Z");
    }
}
