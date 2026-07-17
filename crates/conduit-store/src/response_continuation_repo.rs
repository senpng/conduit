use chrono::{Duration, Utc};
use sqlx::SqlitePool;
use tracing::instrument;

use crate::StoreError;

/// Lifetime of a persisted Responses transcript continuation.
///
/// The entry is compatibility state, not conversation history.  It is long
/// enough for normal tool execution/retries, while keeping retained tool
/// arguments bounded.
pub const RESPONSE_CONTINUATION_TTL: Duration = Duration::hours(1);

#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct ResponseContinuation {
    pub input_items_json: String,
    pub output_items_json: String,
}

/// SQLite-backed short-lived state used to replay Responses transcripts.
pub struct ResponseContinuationRepo<'a> {
    pool: &'a SqlitePool,
}

impl<'a> ResponseContinuationRepo<'a> {
    pub fn new(pool: &'a SqlitePool) -> Self {
        Self { pool }
    }

    /// Persist the complete wire transcript needed for the next Responses turn
    /// and opportunistically remove expired rows in the same transaction.
    #[instrument(skip(self, input_items_json, output_items_json))]
    pub async fn put(
        &self,
        response_id: &str,
        key_scope: &str,
        input_items_json: &str,
        output_items_json: &str,
    ) -> Result<(), StoreError> {
        let now = Utc::now();
        let now = now.to_rfc3339();
        let expires_at = (Utc::now() + RESPONSE_CONTINUATION_TTL).to_rfc3339();
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| StoreError::Sqlx(e.to_string()))?;

        sqlx::query("DELETE FROM response_continuations WHERE expires_at <= ?")
            .bind(&now)
            .execute(&mut *tx)
            .await
            .map_err(|e| StoreError::Sqlx(e.to_string()))?;

        sqlx::query(
            r#"INSERT INTO response_continuations
                   (response_id, key_scope, input_items_json, output_items_json, expires_at, created_at)
               VALUES (?, ?, ?, ?, ?, ?)
               ON CONFLICT(response_id, key_scope) DO UPDATE SET
                   input_items_json = excluded.input_items_json,
                   output_items_json = excluded.output_items_json,
                   expires_at = excluded.expires_at,
                   created_at = excluded.created_at"#,
        )
        .bind(response_id)
        .bind(key_scope)
        .bind(input_items_json)
        .bind(output_items_json)
        .bind(&expires_at)
        .bind(&now)
        .execute(&mut *tx)
        .await
        .map_err(|e| StoreError::Sqlx(e.to_string()))?;

        tx.commit()
            .await
            .map_err(|e| StoreError::Sqlx(e.to_string()))?;
        Ok(())
    }

    /// Return a live continuation for one downstream identity.
    #[instrument(skip(self))]
    pub async fn get(
        &self,
        response_id: &str,
        key_scope: &str,
    ) -> Result<Option<ResponseContinuation>, StoreError> {
        let now = Utc::now().to_rfc3339();
        sqlx::query_as(
            "SELECT input_items_json, output_items_json FROM response_continuations \
             WHERE response_id = ? AND key_scope = ? AND expires_at > ?",
        )
        .bind(response_id)
        .bind(key_scope)
        .bind(now)
        .fetch_optional(self.pool)
        .await
        .map_err(|e| StoreError::Sqlx(e.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::open_db;

    #[tokio::test]
    async fn stores_by_key_scope_and_purges_expired_entries_on_write() {
        let pool = open_db("sqlite::memory:").await.unwrap();
        let repo = ResponseContinuationRepo::new(&pool);
        repo.put(
            "resp_1",
            "key_a",
            r#"[{"type":"message"}]"#,
            r#"[{"type":"message"}]"#,
        )
        .await
        .unwrap();
        assert!(repo.get("resp_1", "key_a").await.unwrap().is_some());
        assert!(repo.get("resp_1", "key_b").await.unwrap().is_none());

        sqlx::query("UPDATE response_continuations SET expires_at = ? WHERE response_id = ?")
            .bind((Utc::now() - Duration::seconds(1)).to_rfc3339())
            .bind("resp_1")
            .execute(&pool)
            .await
            .unwrap();
        repo.put(
            "resp_2",
            "key_a",
            r#"[{"type":"message"}]"#,
            r#"[{"type":"message"}]"#,
        )
        .await
        .unwrap();

        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM response_continuations")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(count, 1);
        assert!(repo.get("resp_2", "key_a").await.unwrap().is_some());
    }

    #[tokio::test]
    async fn writes_after_upgrading_a_legacy_tool_only_table() {
        let pool = sqlx::SqlitePool::connect("sqlite::memory:").await.unwrap();
        sqlx::query(
            r#"CREATE TABLE response_continuations (
                response_id TEXT NOT NULL,
                key_scope TEXT NOT NULL,
                function_calls_json TEXT NOT NULL,
                expires_at TEXT NOT NULL,
                created_at TEXT NOT NULL,
                PRIMARY KEY (response_id, key_scope)
            )"#,
        )
        .execute(&pool)
        .await
        .unwrap();
        crate::run_migrations(&pool).await.unwrap();

        let columns = sqlx::query("PRAGMA table_info(response_continuations)")
            .fetch_all(&pool)
            .await
            .unwrap();
        assert!(!columns.iter().any(|row| {
            use sqlx::Row;
            row.get::<String, _>("name") == "function_calls_json"
        }));

        ResponseContinuationRepo::new(&pool)
            .put("resp_legacy", "key", "[]", "[]")
            .await
            .unwrap();
    }
}
