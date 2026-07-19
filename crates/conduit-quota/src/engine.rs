use std::sync::Arc;

use async_trait::async_trait;
use conduit_ir::error::QuotaError;
use tracing::debug;

use crate::check::{QuotaCheckRequest, QuotaRecordRequest, RecordFn};

// ---------------------------------------------------------------------------
// QuotaEngine trait
// ---------------------------------------------------------------------------

/// Core abstraction for rate-limit check + usage recording.
///
/// Two concerns are separated:
///
/// * **`check`** — may reject the request before it reaches the upstream (RPM).
/// * **`record`** — called after a request finishes to persist the ledger row
///   (tokens, cost, outcome, timing, routing attempts).
///
/// Implementations must be `Send + Sync` so they can be shared across async
/// tasks via `Arc<dyn QuotaEngine>`.
#[async_trait]
pub trait QuotaEngine: Send + Sync {
    /// Return `Ok(())` if the request is within rate limits, or an
    /// appropriate [`QuotaError`] variant if it should be rejected.
    async fn check(&self, req: &QuotaCheckRequest) -> Result<(), QuotaError>;

    /// Persist a finished request's ledger entry (always, including zero-token
    /// success and terminal failures).
    async fn record(&self, req: &QuotaRecordRequest) -> Result<(), QuotaError>;
}

// ---------------------------------------------------------------------------
// NoopQuotaEngine — for testing / disabled quota
// ---------------------------------------------------------------------------

/// A quota engine that always allows requests and silently discards records.
/// Useful in tests and configurations where quota enforcement is disabled.
pub struct NoopQuotaEngine;

#[async_trait]
impl QuotaEngine for NoopQuotaEngine {
    async fn check(&self, _req: &QuotaCheckRequest) -> Result<(), QuotaError> {
        Ok(())
    }

    async fn record(&self, _req: &QuotaRecordRequest) -> Result<(), QuotaError> {
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// InMemoryQuotaEngine — in-process RPM + pluggable record function
// ---------------------------------------------------------------------------

use crate::bucket::SlidingWindowCounter;

/// A [`QuotaEngine`] that enforces RPM limits via an in-memory sliding-window
/// counter and delegates usage persistence to a user-supplied async function.
///
/// This is the primary implementation for production use: RPM state lives in
/// memory (fast, no DB round-trip) while the usage ledger is owned by the
/// storage layer and injected via the `record_fn` closure.
pub struct InMemoryQuotaEngine {
    rpm_counter: Arc<SlidingWindowCounter>,
    record_fn: RecordFn,
}

impl InMemoryQuotaEngine {
    /// Create a new engine.
    ///
    /// * `record_fn` — async function that writes one usage row (or no-ops).
    pub fn new(record_fn: RecordFn) -> Self {
        Self {
            rpm_counter: Arc::new(SlidingWindowCounter::new()),
            record_fn,
        }
    }

    /// Expose the shared RPM counter so callers can wire it into a background
    /// cleanup task.
    pub fn rpm_counter(&self) -> Arc<SlidingWindowCounter> {
        self.rpm_counter.clone()
    }
}

#[async_trait]
impl QuotaEngine for InMemoryQuotaEngine {
    async fn check(&self, req: &QuotaCheckRequest) -> Result<(), QuotaError> {
        if let Some(limit) = req.rate_limit_rpm {
            if !self
                .rpm_counter
                .check_and_increment(&req.downstream_key_id, limit as u64)
                .await
            {
                debug!(
                    key_id = %req.downstream_key_id,
                    alias = %req.model_alias,
                    limit,
                    "quota rpm exceeded"
                );
                return Err(QuotaError::RateLimitExceeded {
                    requests_per_minute: limit,
                });
            }
            debug!(
                key_id = %req.downstream_key_id,
                alias = %req.model_alias,
                limit,
                "quota rpm check ok"
            );
        } else {
            debug!(
                key_id = %req.downstream_key_id,
                alias = %req.model_alias,
                "quota check skipped (no rpm limit)"
            );
        }
        Ok(())
    }

    async fn record(&self, req: &QuotaRecordRequest) -> Result<(), QuotaError> {
        // Always persist: request ledger needs zero-token success and errors
        // for success-rate / latency / routing observability.
        debug!(
            request_id = %req.request_id,
            key_id = %req.downstream_key_id,
            alias = req.alias.as_deref().unwrap_or(""),
            provider_id = req.provider_id.as_deref().unwrap_or(""),
            status = %req.status,
            stream = req.stream,
            prompt_tokens = req.prompt_tokens,
            completion_tokens = req.completion_tokens,
            total_tokens = req.total_tokens,
            cost_usd = req.cost_usd,
            duration_ms = ?req.duration_ms,
            attempt_no = req.attempt_no,
            attempt_count = req.attempt_count,
            "quota record"
        );
        (self.record_fn)(req.clone()).await
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn make_engine_always_ok() -> InMemoryQuotaEngine {
        InMemoryQuotaEngine::new(Arc::new(|_| Box::pin(async { Ok(()) })))
    }

    fn make_req(rpm: Option<u32>) -> QuotaCheckRequest {
        QuotaCheckRequest {
            downstream_key_id: "dk-engine-test".into(),
            rate_limit_rpm: rpm,
            model_alias: "gpt-4o".into(),
        }
    }

    fn sample_record() -> QuotaRecordRequest {
        QuotaRecordRequest {
            request_id: "r1".into(),
            downstream_key_id: "dk".into(),
            alias: Some("gpt".into()),
            provider_id: None,
            provider_kind: Some("openai".into()),
            model_id: Some("gpt-4o".into()),
            prompt_tokens: 10,
            completion_tokens: 5,
            total_tokens: 15,
            reasoning_tokens: 0,
            cache_read_tokens: 0,
            cache_write_tokens: 0,
            cost_usd: 0.05,
            stream: false,
            status: "ok".into(),
            error_class: None,
            http_status: None,
            finish_reason: None,
            duration_ms: Some(42),
            ttfb_ms: Some(10),
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
    async fn noop_engine_always_passes() {
        let engine = NoopQuotaEngine;
        let req = make_req(Some(1));
        assert!(engine.check(&req).await.is_ok());
    }

    #[tokio::test]
    async fn in_memory_engine_passes_within_limits() {
        let engine = make_engine_always_ok();
        let req = make_req(Some(100));
        assert!(engine.check(&req).await.is_ok());
    }

    #[tokio::test]
    async fn in_memory_engine_enforces_rpm() {
        let engine = make_engine_always_ok();
        let req = make_req(Some(2));
        engine.check(&req).await.unwrap();
        engine.check(&req).await.unwrap();
        let result = engine.check(&req).await;
        assert!(matches!(result, Err(QuotaError::RateLimitExceeded { .. })));
    }

    #[tokio::test]
    async fn concurrent_checks_do_not_exceed_rpm_limit() {
        let engine = Arc::new(make_engine_always_ok());
        let req = make_req(Some(1));
        let checks: Vec<_> = (0..100)
            .map(|_| {
                let engine = engine.clone();
                let req = req.clone();
                tokio::spawn(async move { engine.check(&req).await.is_ok() })
            })
            .collect();

        let mut allowed = 0;
        for check in checks {
            allowed += check.await.expect("task should not panic") as u32;
        }
        assert_eq!(allowed, 1);
    }

    #[tokio::test]
    async fn record_propagates_to_record_fn() {
        use std::sync::atomic::{AtomicU64, Ordering};
        let called = Arc::new(AtomicU64::new(0));
        let called2 = called.clone();
        let engine = InMemoryQuotaEngine::new(Arc::new(move |_| {
            let c = called2.clone();
            Box::pin(async move {
                c.fetch_add(1, Ordering::SeqCst);
                Ok(())
            })
        }));
        engine.record(&sample_record()).await.unwrap();
        assert_eq!(called.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn record_always_persists_zero_consumption() {
        use std::sync::atomic::{AtomicU64, Ordering};
        let called = Arc::new(AtomicU64::new(0));
        let called2 = called.clone();
        let engine = InMemoryQuotaEngine::new(Arc::new(move |_| {
            let c = called2.clone();
            Box::pin(async move {
                c.fetch_add(1, Ordering::SeqCst);
                Ok(())
            })
        }));
        let mut zero = sample_record();
        zero.prompt_tokens = 0;
        zero.completion_tokens = 0;
        zero.total_tokens = 0;
        zero.cost_usd = 0.0;
        engine.record(&zero).await.unwrap();
        assert_eq!(called.load(Ordering::SeqCst), 1);
    }
}
