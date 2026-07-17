use std::{future::Future, pin::Pin, sync::Arc};

use conduit_ir::error::QuotaError;

use crate::bucket::SlidingWindowCounter;

// ---------------------------------------------------------------------------
// Shared future alias
// ---------------------------------------------------------------------------

pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Async function that records completed-request consumption.
/// Signature: `(QuotaRecordRequest) -> Result<(), QuotaError>`
pub type RecordFn =
    Arc<dyn Fn(QuotaRecordRequest) -> BoxFuture<'static, Result<(), QuotaError>> + Send + Sync>;

// ---------------------------------------------------------------------------
// QuotaCheckRequest / QuotaRecordRequest
// ---------------------------------------------------------------------------

/// Input for [`QuotaChecker::check`] / [`QuotaEngine::check`].
#[derive(Debug, Clone)]
pub struct QuotaCheckRequest {
    /// Opaque identifier for the downstream API key.
    pub downstream_key_id: String,
    /// Requests-per-minute cap, if any.
    pub rate_limit_rpm: Option<u32>,
    /// The virtual model alias being requested.
    pub model_alias: String,
}

/// Input for [`QuotaEngine::record`] — one completed request's consumption.
///
/// Written to the durable usage ledger by the daemon; independent of traces.
#[derive(Debug, Clone)]
pub struct QuotaRecordRequest {
    /// Stable request id (pipeline `trace_id` / correlation id).
    pub request_id: String,
    /// Opaque identifier for the downstream API key.
    pub downstream_key_id: String,
    /// Client-facing route alias, if known.
    pub alias: Option<String>,
    pub provider_id: Option<String>,
    pub provider_kind: Option<String>,
    pub model_id: Option<String>,
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    pub total_tokens: u32,
    pub reasoning_tokens: u32,
    pub cache_read_tokens: u32,
    pub cache_write_tokens: u32,
    /// Cost of the completed request in USD.
    pub cost_usd: f64,
    pub stream: bool,
}

impl QuotaRecordRequest {
    /// True when the request has any token or cost signal worth recording.
    pub fn has_consumption(&self) -> bool {
        self.prompt_tokens > 0
            || self.completion_tokens > 0
            || self.total_tokens > 0
            || self.reasoning_tokens > 0
            || self.cache_read_tokens > 0
            || self.cache_write_tokens > 0
            || self.cost_usd > 0.0
    }
}

// ---------------------------------------------------------------------------
// QuotaChecker
// ---------------------------------------------------------------------------

/// In-memory RPM enforcement only (budget limits have been removed).
pub struct QuotaChecker {
    /// Shared in-memory sliding-window counter for RPM enforcement.
    pub rpm_counter: Arc<SlidingWindowCounter>,
}

impl QuotaChecker {
    /// Construct a new checker with the given RPM counter.
    pub fn new(rpm_counter: Arc<SlidingWindowCounter>) -> Self {
        Self { rpm_counter }
    }

    /// Check whether `req` is within the RPM limit.
    ///
    /// On success the RPM counter for this key is incremented atomically.
    /// On `Err` no side effects are applied.
    pub async fn check(&self, req: &QuotaCheckRequest) -> Result<(), QuotaError> {
        if let Some(limit) = req.rate_limit_rpm {
            let current = self.rpm_counter.get(&req.downstream_key_id).await;
            if current >= limit as u64 {
                return Err(QuotaError::RateLimitExceeded {
                    requests_per_minute: limit,
                });
            }
        }

        if req.rate_limit_rpm.is_some() {
            self.rpm_counter.increment(&req.downstream_key_id).await;
        }

        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn make_req(rpm: Option<u32>) -> QuotaCheckRequest {
        QuotaCheckRequest {
            downstream_key_id: "dk-test".into(),
            rate_limit_rpm: rpm,
            model_alias: "gpt-4o".into(),
        }
    }

    #[tokio::test]
    async fn passes_with_no_limits() {
        let checker = QuotaChecker::new(Arc::new(SlidingWindowCounter::new()));
        let req = make_req(None);
        assert!(checker.check(&req).await.is_ok());
    }

    #[tokio::test]
    async fn rpm_enforced_at_limit() {
        let counter = Arc::new(SlidingWindowCounter::new());
        for _ in 0..5u32 {
            counter.increment("dk-test").await;
        }
        let checker = QuotaChecker::new(counter);
        let req = make_req(Some(5));
        let result = checker.check(&req).await;
        assert!(
            matches!(result, Err(QuotaError::RateLimitExceeded { .. })),
            "should be rate-limited"
        );
    }

    #[tokio::test]
    async fn rpm_incremented_on_success() {
        let counter = Arc::new(SlidingWindowCounter::new());
        let checker = QuotaChecker::new(counter.clone());
        let req = make_req(Some(10));
        checker.check(&req).await.unwrap();
        checker.check(&req).await.unwrap();
        assert_eq!(counter.get("dk-test").await, 2);
    }
}
