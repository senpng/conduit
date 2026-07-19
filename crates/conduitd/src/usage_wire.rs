//! Usage ledger wiring used by the daemon quota engine.
//!
//! Records every finished request (tokens, cost, outcome, timing, routing) into
//! `usage_records` and optional per-try rows into `usage_attempts`.

use std::sync::Arc;

use conduit_ir::error::QuotaError;
use conduit_quota::check::{QuotaRecordRequest, RecordFn};
use conduit_store::{new_usage_attempt, new_usage_record, StorePool, UsageRepo};

/// Build the `record_fn` injected into [`InMemoryQuotaEngine`](conduit_quota::InMemoryQuotaEngine).
///
/// Always inserts a main row (including zero-token / error outcomes). DB errors
/// fail closed (propagated as [`QuotaError::Backend`]).
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

            let mut row = new_usage_record(
                req.request_id.clone(),
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
            row.status = req.status;
            row.error_class = req.error_class;
            row.http_status = req.http_status;
            row.finish_reason = req.finish_reason;
            row.duration_ms = req.duration_ms;
            row.ttfb_ms = req.ttfb_ms;
            row.route_strategy = req.route_strategy;
            row.attempt_no = req.attempt_no;
            row.attempt_count = req.attempt_count.max(1);
            row.session_id = req.session_id;
            row.affinity_hit = req.affinity_hit;
            row.pool_id = req.pool_id;
            row.selected_reason = req.selected_reason;

            let repo = UsageRepo::new(&pool);
            repo.insert(&row)
                .await
                .map_err(|e| QuotaError::Backend(format!("usage record: {e}")))?;

            for a in req.attempts {
                let attempt = new_usage_attempt(
                    req.request_id.clone(),
                    a.attempt_no,
                    a.provider_id,
                    a.provider_kind,
                    a.model_id,
                    a.status,
                    a.error_class,
                    a.http_status,
                    a.duration_ms,
                    a.ttfb_ms,
                    a.reason,
                );
                repo.insert_attempt(&attempt)
                    .await
                    .map_err(|e| QuotaError::Backend(format!("usage attempt: {e}")))?;
            }

            Ok(())
        })
    })
}

#[cfg(test)]
mod tests {
    use conduit_quota::QuotaAttemptRecord;
    use conduit_store::open_db;

    use super::*;

    fn base_req() -> QuotaRecordRequest {
        QuotaRecordRequest {
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
            status: "ok".into(),
            error_class: None,
            http_status: None,
            finish_reason: Some("stop".into()),
            duration_ms: Some(100),
            ttfb_ms: Some(25),
            route_strategy: Some("fixed".into()),
            attempt_no: 0,
            attempt_count: 1,
            session_id: None,
            affinity_hit: None,
            pool_id: None,
            selected_reason: Some("fixed".into()),
            attempts: Vec::new(),
        }
    }

    #[tokio::test]
    async fn record_writes_usage_row() {
        let pool = open_db("sqlite::memory:").await.unwrap();
        let record_fn = make_record_fn(pool.clone());

        (record_fn)(base_req()).await.unwrap();

        let rows = UsageRepo::new(&pool).list(10, None, None).await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].request_id, "tr-1");
        assert_eq!(rows[0].downstream_key_id.as_deref(), Some("dk_abc"));
        assert!((rows[0].cost_usd - 0.02).abs() < 1e-9);
        assert_eq!(rows[0].status, "ok");
        assert_eq!(rows[0].duration_ms, Some(100));
        assert_eq!(rows[0].ttfb_ms, Some(25));
        assert_eq!(rows[0].finish_reason.as_deref(), Some("stop"));
    }

    #[tokio::test]
    async fn zero_token_and_error_rows_persist() {
        let pool = open_db("sqlite::memory:").await.unwrap();
        let record_fn = make_record_fn(pool.clone());

        let mut zero = base_req();
        zero.request_id = "tr-zero".into();
        zero.prompt_tokens = 0;
        zero.completion_tokens = 0;
        zero.total_tokens = 0;
        zero.cost_usd = 0.0;
        zero.ttfb_ms = None;
        (record_fn)(zero).await.unwrap();

        let mut err = base_req();
        err.request_id = "tr-err".into();
        err.prompt_tokens = 0;
        err.completion_tokens = 0;
        err.total_tokens = 0;
        err.cost_usd = 0.0;
        err.status = "error".into();
        err.error_class = Some("rate_limited".into());
        err.http_status = Some(429);
        err.finish_reason = None;
        err.ttfb_ms = None;
        err.attempt_count = 2;
        err.attempts = vec![
            QuotaAttemptRecord {
                attempt_no: 0,
                provider_id: Some("p0".into()),
                provider_kind: Some("openai".into()),
                model_id: Some("gpt-4o".into()),
                status: "error".into(),
                error_class: Some("rate_limited".into()),
                http_status: Some(429),
                duration_ms: Some(10),
                ttfb_ms: None,
                reason: Some("initial".into()),
            },
            QuotaAttemptRecord {
                attempt_no: 1,
                provider_id: Some("p1".into()),
                provider_kind: Some("openai".into()),
                model_id: Some("gpt-4o".into()),
                status: "error".into(),
                error_class: Some("rate_limited".into()),
                http_status: Some(429),
                duration_ms: Some(20),
                ttfb_ms: None,
                reason: Some("retry".into()),
            },
        ];
        (record_fn)(err).await.unwrap();

        let repo = UsageRepo::new(&pool);
        let rows = repo.list(10, None, None).await.unwrap();
        assert_eq!(rows.len(), 2);
        let zero_row = rows.iter().find(|r| r.request_id == "tr-zero").unwrap();
        assert_eq!(zero_row.status, "ok");
        assert_eq!(zero_row.total_tokens, 0);
        let err_row = rows.iter().find(|r| r.request_id == "tr-err").unwrap();
        assert_eq!(err_row.status, "error");
        assert_eq!(err_row.error_class.as_deref(), Some("rate_limited"));

        let attempts = repo.list_attempts("tr-err").await.unwrap();
        assert_eq!(attempts.len(), 2);
        assert_eq!(attempts[0].provider_id.as_deref(), Some("p0"));
        assert_eq!(attempts[1].reason.as_deref(), Some("retry"));
    }

    #[tokio::test]
    async fn anonymous_key_stored_as_null() {
        let pool = open_db("sqlite::memory:").await.unwrap();
        let record_fn = make_record_fn(pool.clone());

        let mut req = base_req();
        req.request_id = "tr-2".into();
        req.downstream_key_id = "_anonymous".into();
        req.alias = None;
        req.provider_id = None;
        req.provider_kind = None;
        req.model_id = None;
        req.prompt_tokens = 1;
        req.completion_tokens = 0;
        req.total_tokens = 1;
        req.cost_usd = 0.0;
        req.stream = true;
        req.finish_reason = None;
        req.duration_ms = Some(5);
        req.ttfb_ms = Some(2);
        req.route_strategy = None;
        req.selected_reason = None;
        (record_fn)(req).await.unwrap();

        let rows = UsageRepo::new(&pool).list(10, None, None).await.unwrap();
        assert_eq!(rows.len(), 1);
        assert!(rows[0].downstream_key_id.is_none());
        assert!(rows[0].stream);
    }

    #[tokio::test]
    async fn db_error_fails_closed() {
        let pool = open_db("sqlite::memory:").await.unwrap();
        pool.close().await;
        let record_fn = make_record_fn(pool);
        let err = (record_fn)(base_req())
            .await
            .expect_err("must fail closed");
        assert!(matches!(err, QuotaError::Backend(_)), "got {err:?}");
    }
}
