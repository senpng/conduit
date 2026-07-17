//! Usage ledger wiring used by the daemon quota engine.
//!
//! Records every request's token + cost consumption into `usage_records`.
//! Independent of the trace log so spend survives when tracing is disabled.

use std::sync::Arc;

use conduit_ir::error::QuotaError;
use conduit_quota::check::{QuotaRecordRequest, RecordFn};
use conduit_store::{new_usage_record, StorePool, UsageRepo};

/// Build the `record_fn` injected into [`InMemoryQuotaEngine`](conduit_quota::InMemoryQuotaEngine).
///
/// Always inserts a row when the engine forwards a non-empty record.
/// DB errors fail closed (propagated as [`QuotaError::Backend`]).
pub fn make_record_fn(pool: StorePool) -> RecordFn {
    Arc::new(move |req: QuotaRecordRequest| {
        let pool = pool.clone();
        Box::pin(async move {
            let key_id = {
                let id = req.downstream_key_id.trim();
                if id.is_empty() || id == "_anonymous" || id == "_local" {
                    None
                } else {
                    Some(id.to_string())
                }
            };

            let row = new_usage_record(
                req.request_id,
                key_id,
                req.alias,
                req.provider_id,
                req.provider_kind,
                req.model_id,
                req.prompt_tokens,
                req.completion_tokens,
                req.total_tokens,
                req.reasoning_tokens,
                req.cache_read_tokens,
                req.cache_write_tokens,
                req.cost_usd,
                req.stream,
            );

            let repo = UsageRepo::new(&pool);
            repo.insert(&row)
                .await
                .map_err(|e| QuotaError::Backend(format!("usage record: {e}")))
        })
    })
}

#[cfg(test)]
mod tests {
    use conduit_store::open_db;

    use super::*;

    #[tokio::test]
    async fn record_writes_usage_row() {
        let pool = open_db("sqlite::memory:").await.unwrap();
        let record_fn = make_record_fn(pool.clone());

        (record_fn)(QuotaRecordRequest {
            request_id: "tr-1".into(),
            downstream_key_id: "dk_abc".into(),
            alias: Some("gpt".into()),
            provider_id: Some("p1".into()),
            provider_kind: Some("openai".into()),
            model_id: Some("gpt-4o".into()),
            prompt_tokens: 10,
            completion_tokens: 5,
            total_tokens: 15,
            reasoning_tokens: 0,
            cache_read_tokens: 0,
            cache_write_tokens: 0,
            cost_usd: 0.02,
            stream: false,
        })
        .await
        .unwrap();

        let rows = UsageRepo::new(&pool).list(10, None).await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].request_id, "tr-1");
        assert_eq!(rows[0].downstream_key_id.as_deref(), Some("dk_abc"));
        assert!((rows[0].cost_usd - 0.02).abs() < 1e-9);
    }

    #[tokio::test]
    async fn anonymous_key_stored_as_null() {
        let pool = open_db("sqlite::memory:").await.unwrap();
        let record_fn = make_record_fn(pool.clone());

        (record_fn)(QuotaRecordRequest {
            request_id: "tr-2".into(),
            downstream_key_id: "_anonymous".into(),
            alias: None,
            provider_id: None,
            provider_kind: None,
            model_id: None,
            prompt_tokens: 1,
            completion_tokens: 0,
            total_tokens: 1,
            reasoning_tokens: 0,
            cache_read_tokens: 0,
            cache_write_tokens: 0,
            cost_usd: 0.0,
            stream: true,
        })
        .await
        .unwrap();

        let rows = UsageRepo::new(&pool).list(10, None).await.unwrap();
        assert_eq!(rows.len(), 1);
        assert!(rows[0].downstream_key_id.is_none());
        assert!(rows[0].stream);
    }

    #[tokio::test]
    async fn db_error_fails_closed() {
        let pool = open_db("sqlite::memory:").await.unwrap();
        pool.close().await;
        let record_fn = make_record_fn(pool);
        let err = (record_fn)(QuotaRecordRequest {
            request_id: "x".into(),
            downstream_key_id: "k".into(),
            alias: None,
            provider_id: None,
            provider_kind: None,
            model_id: None,
            prompt_tokens: 1,
            completion_tokens: 0,
            total_tokens: 1,
            reasoning_tokens: 0,
            cache_read_tokens: 0,
            cache_write_tokens: 0,
            cost_usd: 0.01,
            stream: false,
        })
        .await
        .expect_err("must fail closed");
        assert!(matches!(err, QuotaError::Backend(_)), "got {err:?}");
    }
}
