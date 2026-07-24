//! Insert / ledger write path.

use tracing::instrument;

use crate::{
    schema::{UsageAttemptRow, UsageRecordRow},
    StoreError,
};

use super::map::{insert_attempt_on, insert_row_on, map_attempt_row};
use super::UsageRepo;

impl<'a> UsageRepo<'a> {
    /// Insert one request ledger row (success, zero-token, or terminal failure).
    #[instrument(skip(self, row))]
    pub async fn insert(&self, row: &UsageRecordRow) -> Result<(), StoreError> {
        self.insert_ledger(row, &[]).await
    }

    /// Insert one per-try attempt row.
    #[instrument(skip(self, row))]
    pub async fn insert_attempt(&self, row: &UsageAttemptRow) -> Result<(), StoreError> {
        insert_attempt_on(self.pool, row).await
    }

    /// Insert a main row + optional attempts in a single transaction.
    #[instrument(skip(self, row, attempts))]
    pub async fn insert_ledger(
        &self,
        row: &UsageRecordRow,
        attempts: &[UsageAttemptRow],
    ) -> Result<(), StoreError> {
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| StoreError::Sqlx(e.to_string()))?;
        insert_row_on(&mut *tx, row).await?;
        for a in attempts {
            insert_attempt_on(&mut *tx, a).await?;
        }
        tx.commit()
            .await
            .map_err(|e| StoreError::Sqlx(e.to_string()))?;
        Ok(())
    }

    /// Insert many ledger entries in one transaction (async writer batching).
    #[instrument(skip(self, items))]
    pub async fn insert_ledger_batch(
        &self,
        items: &[(UsageRecordRow, Vec<UsageAttemptRow>)],
    ) -> Result<(), StoreError> {
        if items.is_empty() {
            return Ok(());
        }
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| StoreError::Sqlx(e.to_string()))?;
        for (row, attempts) in items {
            insert_row_on(&mut *tx, row).await?;
            for a in attempts {
                insert_attempt_on(&mut *tx, a).await?;
            }
        }
        tx.commit()
            .await
            .map_err(|e| StoreError::Sqlx(e.to_string()))?;
        Ok(())
    }

    /// List attempt rows for a request (ordered by attempt_no).
    #[instrument(skip(self))]
    pub async fn list_attempts(
        &self,
        request_id: &str,
    ) -> Result<Vec<UsageAttemptRow>, StoreError> {
        let rows = sqlx::query(
            r#"SELECT id, request_id, attempt_no, provider_id, provider_kind, model_id,
                      status, error_class, http_status, duration_ms, ttfb_ms, reason, ts
               FROM usage_attempts
               WHERE request_id = ?
               ORDER BY attempt_no ASC"#,
        )
        .bind(request_id)
        .fetch_all(self.pool)
        .await
        .map_err(|e| StoreError::Sqlx(e.to_string()))?;
        Ok(rows.into_iter().map(map_attempt_row).collect())
    }

}
