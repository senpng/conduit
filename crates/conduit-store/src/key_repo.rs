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
               (id, name, key_hash, model_whitelist, monthly_budget_usd, rate_limit_rpm, enabled, created_at, updated_at)
               VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)"#,
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
        .execute(self.pool)
        .await
        .map_err(|e| StoreError::Sqlx(e.to_string()))?;
        Ok(())
    }

    #[instrument(skip(self))]
    pub async fn get(&self, id: &str) -> Result<Option<DownstreamKeyRow>, StoreError> {
        let row = sqlx::query(
            "SELECT id, name, key_hash, model_whitelist, monthly_budget_usd, rate_limit_rpm, enabled, created_at, updated_at
             FROM downstream_keys WHERE id = ?",
        )
        .bind(id)
        .fetch_optional(self.pool)
        .await
        .map_err(|e| StoreError::Sqlx(e.to_string()))?
        .map(map_key_row);
        Ok(row)
    }

    /// Look up a key by its BLAKE3 hash for fast authentication.
    #[instrument(skip(self))]
    pub async fn get_by_hash(&self, hash: &str) -> Result<Option<DownstreamKeyRow>, StoreError> {
        let row = sqlx::query(
            "SELECT id, name, key_hash, model_whitelist, monthly_budget_usd, rate_limit_rpm, enabled, created_at, updated_at
             FROM downstream_keys WHERE key_hash = ? AND enabled = 1",
        )
        .bind(hash)
        .fetch_optional(self.pool)
        .await
        .map_err(|e| StoreError::Sqlx(e.to_string()))?
        .map(map_key_row);
        Ok(row)
    }

    #[instrument(skip(self))]
    pub async fn list(&self) -> Result<Vec<DownstreamKeyRow>, StoreError> {
        let rows = sqlx::query(
            "SELECT id, name, key_hash, model_whitelist, monthly_budget_usd, rate_limit_rpm, enabled, created_at, updated_at
             FROM downstream_keys ORDER BY created_at DESC",
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
        sqlx::query("UPDATE downstream_keys SET enabled = ?, updated_at = ? WHERE id = ?")
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
            "UPDATE downstream_keys SET rate_limit_rpm = ?, updated_at = ? WHERE id = ?",
        )
        .bind(rate_limit_rpm)
        .bind(&now)
        .bind(id)
        .execute(self.pool)
        .await
        .map_err(|e| StoreError::Sqlx(e.to_string()))?;
        Ok(())
    }

    #[instrument(skip(self))]
    pub async fn delete(&self, id: &str) -> Result<(), StoreError> {
        sqlx::query("DELETE FROM downstream_keys WHERE id = ?")
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
    }

    #[tokio::test]
    async fn delete_removes_key() {
        let pool = open_db("sqlite::memory:").await.unwrap();
        let repo = KeyRepo::new(&pool);

        repo.insert(&make_key("k3", "ghi789")).await.unwrap();
        repo.delete("k3").await.unwrap();
        assert!(repo.get("k3").await.unwrap().is_none());
    }
}
