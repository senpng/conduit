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
               (id, name, kind, base_url, upstream_key_ref, created_at, updated_at)
               VALUES (?, ?, ?, ?, ?, ?, ?)"#,
        )
        .bind(&row.id)
        .bind(&row.name)
        .bind(&row.kind)
        .bind(&row.base_url)
        .bind(&row.upstream_key_ref)
        .bind(&row.created_at)
        .bind(&row.updated_at)
        .execute(self.pool)
        .await
        .map_err(|e| StoreError::Sqlx(e.to_string()))?;
        Ok(())
    }

    #[instrument(skip(self))]
    pub async fn get(&self, id: &str) -> Result<Option<ProviderRow>, StoreError> {
        let row = sqlx::query(
            "SELECT id, name, kind, base_url, upstream_key_ref, created_at, updated_at
             FROM providers WHERE id = ?",
        )
        .bind(id)
        .fetch_optional(self.pool)
        .await
        .map_err(|e| StoreError::Sqlx(e.to_string()))?
        .map(map_provider_row);
        Ok(row)
    }

    #[instrument(skip(self))]
    pub async fn list(&self) -> Result<Vec<ProviderRow>, StoreError> {
        let rows = sqlx::query(
            "SELECT id, name, kind, base_url, upstream_key_ref, created_at, updated_at
             FROM providers ORDER BY created_at ASC",
        )
        .fetch_all(self.pool)
        .await
        .map_err(|e| StoreError::Sqlx(e.to_string()))?
        .into_iter()
        .map(map_provider_row)
        .collect();
        Ok(rows)
    }

    #[instrument(skip(self))]
    pub async fn update(&self, row: &ProviderRow) -> Result<(), StoreError> {
        let updated_at = Utc::now().to_rfc3339();
        sqlx::query(
            r#"UPDATE providers
               SET name = ?, kind = ?, base_url = ?, upstream_key_ref = ?, updated_at = ?
               WHERE id = ?"#,
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

    #[instrument(skip(self))]
    pub async fn delete(&self, id: &str) -> Result<(), StoreError> {
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
        }
    }

    #[tokio::test]
    async fn insert_get_delete() {
        let pool = open_db("sqlite::memory:").await.unwrap();
        let repo = ProviderRepo::new(&pool);

        repo.insert(&make_row("p1")).await.unwrap();
        let got = repo.get("p1").await.unwrap().unwrap();
        assert_eq!(got.kind, "openai");

        repo.delete("p1").await.unwrap();
        assert!(repo.get("p1").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn list_returns_all() {
        let pool = open_db("sqlite::memory:").await.unwrap();
        let repo = ProviderRepo::new(&pool);

        repo.insert(&make_row("a")).await.unwrap();
        repo.insert(&make_row("b")).await.unwrap();

        let rows = repo.list().await.unwrap();
        assert_eq!(rows.len(), 2);
    }
}
