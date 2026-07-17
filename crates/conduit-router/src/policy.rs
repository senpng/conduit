use std::collections::HashSet;

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// RetryPolicy
// ---------------------------------------------------------------------------

/// Controls how the pipeline retries failed upstream attempts.
///
/// Backoff formula: `base_ms * 2^(attempt_no)`, capped at `base_ms * 4`.
///
/// Example (base_ms = 500):
/// - attempt 0 → immediate (no delay before first try)
/// - attempt 1 → 500 ms
/// - attempt 2 → 1000 ms
/// - attempt 3+ → 2000 ms  (capped at 4× base)
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RetryPolicy {
    /// Maximum number of additional attempts (0 = try once, no retries).
    pub max_retries: u32,
    /// Base delay in milliseconds for exponential back-off.
    pub base_delay_ms: u64,
    /// HTTP status codes that should trigger a retry (e.g. `[429, 500, 502, 503, 504]`).
    pub retryable_statuses: HashSet<u16>,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_retries: 2,
            base_delay_ms: 500,
            retryable_statuses: [429, 500, 502, 503, 504].into_iter().collect(),
        }
    }
}

impl RetryPolicy {
    /// No retries at all — try once and fail immediately on error.
    pub fn none() -> Self {
        Self {
            max_retries: 0,
            base_delay_ms: 0,
            retryable_statuses: HashSet::new(),
        }
    }

    /// Compute the back-off delay (in milliseconds) before `attempt_no`.
    ///
    /// - `attempt_no == 0` → 0 ms (no delay before the first attempt)
    /// - `attempt_no == 1` → `base_delay_ms`
    /// - `attempt_no == 2` → `base_delay_ms * 2`
    /// - `attempt_no >= 3` → `base_delay_ms * 4` (cap)
    pub fn delay_ms(&self, attempt_no: u32) -> u64 {
        if attempt_no == 0 {
            return 0;
        }
        let multiplier = 2u64.pow(attempt_no - 1);
        // Cap at 4× base
        let capped = multiplier.min(4);
        self.base_delay_ms * capped
    }

    /// Returns `true` when `attempt_no` is within the retry budget.
    pub fn should_attempt(&self, attempt_no: u32) -> bool {
        attempt_no <= self.max_retries
    }

    /// Returns `true` when the given HTTP status code warrants a retry.
    pub fn should_retry_status(&self, status: u16) -> bool {
        self.retryable_statuses.contains(&status)
    }
}

// ---------------------------------------------------------------------------
// QuotaPolicy
// ---------------------------------------------------------------------------

/// Inputs to the rate-limit check performed before forwarding a request.
///
/// Spend is recorded in the usage ledger; hard monthly budget caps are not
/// enforced at the gateway.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct QuotaPolicy {
    /// Maximum number of requests per minute across all upstreams. `None` = unlimited.
    pub requests_per_minute: Option<u32>,
    /// Maximum tokens (prompt + completion) per calendar month. `None` = unlimited.
    pub monthly_token_quota: Option<u64>,
}

impl QuotaPolicy {
    /// Returns `true` when all limits are disabled (open quota).
    pub fn is_unlimited(&self) -> bool {
        self.requests_per_minute.is_none() && self.monthly_token_quota.is_none()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // -----------------------------------------------------------------------
    // RetryPolicy tests
    // -----------------------------------------------------------------------

    #[test]
    fn default_policy_values() {
        let p = RetryPolicy::default();
        assert_eq!(p.max_retries, 2);
        assert_eq!(p.base_delay_ms, 500);
        assert!(p.retryable_statuses.contains(&429));
        assert!(p.retryable_statuses.contains(&500));
        assert!(p.retryable_statuses.contains(&503));
    }

    #[test]
    fn delay_ms_first_attempt_is_zero() {
        let p = RetryPolicy::default();
        assert_eq!(p.delay_ms(0), 0);
    }

    #[test]
    fn delay_ms_exponential() {
        let p = RetryPolicy {
            base_delay_ms: 500,
            ..Default::default()
        };
        assert_eq!(p.delay_ms(1), 500); // 500 * 2^0
        assert_eq!(p.delay_ms(2), 1000); // 500 * 2^1
        assert_eq!(p.delay_ms(3), 2000); // 500 * 4  (cap)
        assert_eq!(p.delay_ms(4), 2000); // 500 * 4  (cap, no further growth)
        assert_eq!(p.delay_ms(10), 2000); // still capped
    }

    #[test]
    fn should_attempt_within_budget() {
        let p = RetryPolicy {
            max_retries: 2,
            ..Default::default()
        };
        assert!(p.should_attempt(0));
        assert!(p.should_attempt(1));
        assert!(p.should_attempt(2));
        assert!(!p.should_attempt(3));
    }

    #[test]
    fn no_retries_policy() {
        let p = RetryPolicy::none();
        assert!(p.should_attempt(0));
        assert!(!p.should_attempt(1));
        assert_eq!(p.delay_ms(0), 0);
        assert_eq!(p.delay_ms(1), 0); // base is 0
    }

    #[test]
    fn retryable_status_codes() {
        let p = RetryPolicy::default();
        assert!(p.should_retry_status(429));
        assert!(p.should_retry_status(500));
        assert!(p.should_retry_status(502));
        assert!(p.should_retry_status(503));
        assert!(p.should_retry_status(504));
        assert!(!p.should_retry_status(400));
        assert!(!p.should_retry_status(401));
        assert!(!p.should_retry_status(200));
    }

    #[test]
    fn retry_policy_serde_roundtrip() {
        let p = RetryPolicy::default();
        let j = serde_json::to_string(&p).unwrap();
        let back: RetryPolicy = serde_json::from_str(&j).unwrap();
        assert_eq!(p, back);
    }

    // -----------------------------------------------------------------------
    // QuotaPolicy tests
    // -----------------------------------------------------------------------

    #[test]
    fn quota_unlimited_when_no_caps() {
        let q = QuotaPolicy::default();
        assert!(q.is_unlimited());
    }

    #[test]
    fn quota_not_unlimited_with_rpm() {
        let q = QuotaPolicy {
            requests_per_minute: Some(60),
            ..Default::default()
        };
        assert!(!q.is_unlimited());
    }
}
