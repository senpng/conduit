use chrono::Utc;
use sqlx::{Row, SqlitePool};
use tracing::instrument;

use crate::{schema::ProviderRow, StoreError};

pub struct ProviderRepo<'a> {
    pool: &'a SqlitePool,
}

impl<'a> ProviderRepo<'a> {
    pub fn new(pool: &'a SqlitePool) -> Self {
        Self { pool }
    }

    #[instrument(skip(self, row))]
    pub async fn insert(&self, row: &ProviderRow) -> Result<(), StoreError> {
        sqlx::query(
            r#"INSERT INTO providers
               (id, name, kind, base_url, upstream_key_ref, created_at, updated_at, deleted_at)
               VALUES (?, ?, ?, ?, ?, ?, ?, ?)"#,
        )
        .bind(&row.id)
        .bind(&row.name)
        .bind(&row.kind)
        .bind(&row.base_url)
        .bind(&row.upstream_key_ref)
        .bind(&row.created_at)
        .bind(&row.updated_at)
        .bind(&row.deleted_at)
        .execute(self.pool)
        .await
        .map_err(|e| StoreError::Sqlx(e.to_string()))?;
        Ok(())
    }

    /// Active (non-deleted) provider by id.
    #[instrument(skip(self))]
    pub async fn get(&self, id: &str) -> Result<Option<ProviderRow>, StoreError> {
        let row = sqlx::query(
            "SELECT id, name, kind, base_url, upstream_key_ref, created_at, updated_at, deleted_at
             FROM providers WHERE id = ? AND deleted_at IS NULL",
        )
        .bind(id)
        .fetch_optional(self.pool)
        .await
        .map_err(|e| StoreError::Sqlx(e.to_string()))?
        .map(map_provider_row);
        Ok(row)
    }

    /// Active providers only.
    #[instrument(skip(self))]
    pub async fn list(&self) -> Result<Vec<ProviderRow>, StoreError> {
        let rows = sqlx::query(
            "SELECT id, name, kind, base_url, upstream_key_ref, created_at, updated_at, deleted_at
             FROM providers WHERE deleted_at IS NULL ORDER BY created_at ASC",
        )
        .fetch_all(self.pool)
        .await
        .map_err(|e| StoreError::Sqlx(e.to_string()))?
        .into_iter()
        .map(map_provider_row)
        .collect();
        Ok(rows)
    }

    #[instrument(skip(self, row))]
    pub async fn update(&self, row: &ProviderRow) -> Result<(), StoreError> {
        let updated_at = Utc::now().to_rfc3339();
        sqlx::query(
            r#"UPDATE providers
               SET name = ?, kind = ?, base_url = ?, upstream_key_ref = ?, updated_at = ?
               WHERE id = ? AND deleted_at IS NULL"#,
        )
        .bind(&row.name)
        .bind(&row.kind)
        .bind(&row.base_url)
        .bind(&row.upstream_key_ref)
        .bind(&updated_at)
        .bind(&row.id)
        .execute(self.pool)
        .await
        .map_err(|e| StoreError::Sqlx(e.to_string()))?;
        Ok(())
    }

    /// Soft-delete: set `deleted_at`. Secrets and row data are retained.
    #[instrument(skip(self))]
    pub async fn delete(&self, id: &str) -> Result<(), StoreError> {
        let now = Utc::now().to_rfc3339();
        sqlx::query(
            "UPDATE providers SET deleted_at = ?, updated_at = ?
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

    /// Permanently remove a row (create rollback only). Prefer [`delete`].
    #[instrument(skip(self))]
    pub async fn hard_delete(&self, id: &str) -> Result<(), StoreError> {
        sqlx::query("DELETE FROM providers WHERE id = ?")
            .bind(id)
            .execute(self.pool)
            .await
            .map_err(|e| StoreError::Sqlx(e.to_string()))?;
        Ok(())
    }
}

fn map_provider_row(r: sqlx::sqlite::SqliteRow) -> ProviderRow {
    ProviderRow {
        id: r.get("id"),
        name: r.get("name"),
        kind: r.get("kind"),
        base_url: r.get("base_url"),
        upstream_key_ref: r.get("upstream_key_ref"),
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

    fn make_row(id: &str) -> ProviderRow {
        let now = Utc::now().to_rfc3339();
        ProviderRow {
            id: id.to_string(),
            name: "Test Provider".into(),
            kind: "openai".into(),
            base_url: "https://api.openai.com/v1".into(),
            upstream_key_ref: "secret:upstream_key/key-001".into(),
            created_at: now.clone(),
            updated_at: now,
            deleted_at: None,
        }
    }

    #[tokio::test]
    async fn insert_get_soft_delete() {
        let pool = open_db("sqlite::memory:").await.unwrap();
        let repo = ProviderRepo::new(&pool);

        repo.insert(&make_row("p1")).await.unwrap();
        let got = repo.get("p1").await.unwrap().unwrap();
        assert_eq!(got.kind, "openai");
        assert!(got.deleted_at.is_none());

        repo.delete("p1").await.unwrap();
        assert!(repo.get("p1").await.unwrap().is_none());
        assert!(repo.list().await.unwrap().is_empty());

        // Row still exists in DB (soft delete).
        let raw: Option<(String,)> =
            sqlx::query_as("SELECT id FROM providers WHERE id = 'p1'")
                .fetch_optional(&pool)
                .await
                .unwrap();
        assert!(raw.is_some());
    }

    #[tokio::test]
    async fn list_returns_all_active() {
        let pool = open_db("sqlite::memory:").await.unwrap();
        let repo = ProviderRepo::new(&pool);

        repo.insert(&make_row("a")).await.unwrap();
        repo.insert(&make_row("b")).await.unwrap();
        repo.delete("a").await.unwrap();

        let rows = repo.list().await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].id, "b");
    }

    #[tokio::test]
    async fn hard_delete_removes_row() {
        let pool = open_db("sqlite::memory:").await.unwrap();
        let repo = ProviderRepo::new(&pool);
        repo.insert(&make_row("p1")).await.unwrap();
        repo.hard_delete("p1").await.unwrap();
        let raw: Option<(String,)> =
            sqlx::query_as("SELECT id FROM providers WHERE id = 'p1'")
                .fetch_optional(&pool)
                .await
                .unwrap();
        assert!(raw.is_none());
    }
}
