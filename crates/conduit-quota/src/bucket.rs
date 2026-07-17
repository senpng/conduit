use std::{collections::HashMap, sync::Arc};

use tokio::sync::Mutex;

/// A sliding-window request counter keyed by one-minute time buckets.
///
/// Bucket keys take the form `"rpm:{key_id}:{YYYYMMddHHmm}"` so that each
/// (key, minute) pair has its own counter.  Buckets older than 2 minutes are
/// removed by [`cleanup_old_buckets`].
pub struct SlidingWindowCounter {
    counts: Arc<Mutex<HashMap<String, u64>>>,
}

impl SlidingWindowCounter {
    pub fn new() -> Self {
        Self {
            counts: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Increment the counter for `key` in the current minute and return the
    /// updated value.
    pub async fn increment(&self, key: &str) -> u64 {
        let bucket = bucket_key(key);
        let mut guard = self.counts.lock().await;
        let entry = guard.entry(bucket).or_insert(0);
        *entry += 1;
        *entry
    }

    /// Atomically reserve one request when the current bucket is below `limit`.
    /// Returns `true` only when this call incremented the bucket.
    pub async fn check_and_increment(&self, key: &str, limit: u64) -> bool {
        let bucket = bucket_key(key);
        let mut guard = self.counts.lock().await;
        let entry = guard.entry(bucket).or_insert(0);
        if *entry >= limit {
            return false;
        }
        *entry += 1;
        true
    }

    /// Return the counter for `key` in the current minute without changing it.
    pub async fn get(&self, key: &str) -> u64 {
        let bucket = bucket_key(key);
        let guard = self.counts.lock().await;
        guard.get(&bucket).copied().unwrap_or(0)
    }

    /// Drop all buckets whose embedded timestamp is more than 2 minutes in the
    /// past.  Call this periodically (e.g. every 30 s) to bound memory growth.
    pub async fn cleanup_old_buckets(&self) {
        use chrono::Utc;
        let now = Utc::now();
        let mut guard = self.counts.lock().await;
        guard.retain(|key, _| {
            // Expected suffix format: ":{YYYYMMddHHmm}"
            if let Some(ts_str) = key.rsplit(':').next() {
                if ts_str.len() == 12 {
                    if let Ok(naive) = chrono::NaiveDateTime::parse_from_str(
                        &format!("{}00", ts_str),
                        "%Y%m%d%H%M%S",
                    ) {
                        let bucket_utc = naive.and_utc();
                        return (now - bucket_utc).num_minutes() < 2;
                    }
                }
            }
            // Keep anything that doesn't match the expected format.
            true
        });
    }
}

impl Default for SlidingWindowCounter {
    fn default() -> Self {
        Self::new()
    }
}

/// Build the bucket key for `base_key` in the current UTC minute.
fn bucket_key(base_key: &str) -> String {
    let minute = chrono::Utc::now().format("%Y%m%d%H%M");
    format!("rpm:{}:{}", base_key, minute)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;

    #[tokio::test]
    async fn increment_and_get_same_minute() {
        let counter = SlidingWindowCounter::new();
        assert_eq!(counter.get("k1").await, 0);
        counter.increment("k1").await;
        counter.increment("k1").await;
        assert_eq!(counter.get("k1").await, 2);
    }

    #[tokio::test]
    async fn independent_keys_dont_interfere() {
        let counter = SlidingWindowCounter::new();
        counter.increment("k1").await;
        counter.increment("k1").await;
        counter.increment("k2").await;
        assert_eq!(counter.get("k1").await, 2);
        assert_eq!(counter.get("k2").await, 1);
    }

    #[tokio::test]
    async fn cleanup_removes_stale_buckets() {
        let counter = SlidingWindowCounter::new();
        // Manually insert a stale bucket (year 2000 — definitely expired).
        {
            let mut guard = counter.counts.lock().await;
            guard.insert("rpm:stale_key:200001010000".to_string(), 5);
        }
        counter.increment("fresh_key").await;
        counter.cleanup_old_buckets().await;

        let guard = counter.counts.lock().await;
        assert!(
            !guard.contains_key("rpm:stale_key:200001010000"),
            "stale bucket should have been cleaned up"
        );
        // The fresh bucket (current minute) should remain.
        let has_fresh = guard.keys().any(|k| k.starts_with("rpm:fresh_key:"));
        assert!(has_fresh, "fresh bucket should still be present");
    }

    /// Concurrent increments must never exceed the accumulated total.
    #[tokio::test]
    async fn concurrent_increments_are_accurate() {
        let counter = Arc::new(SlidingWindowCounter::new());
        let tasks: Vec<_> = (0..100)
            .map(|_| {
                let c = counter.clone();
                tokio::spawn(async move { c.increment("concurrent_key").await })
            })
            .collect();

        for t in tasks {
            t.await.expect("task should not panic");
        }

        let total = counter.get("concurrent_key").await;
        assert_eq!(total, 100, "all 100 concurrent increments must be recorded");
    }

    #[tokio::test]
    async fn concurrent_check_and_increment_never_exceeds_limit() {
        let counter = Arc::new(SlidingWindowCounter::new());
        let tasks: Vec<_> = (0..100)
            .map(|_| {
                let counter = counter.clone();
                tokio::spawn(async move { counter.check_and_increment("limited_key", 1).await })
            })
            .collect();

        let mut allowed = 0;
        for task in tasks {
            allowed += task.await.expect("task should not panic") as u32;
        }
        assert_eq!(allowed, 1);
        assert_eq!(counter.get("limited_key").await, 1);
    }
}

// ---------------------------------------------------------------------------
// Proptest suite
// ---------------------------------------------------------------------------

#[cfg(test)]
mod proptests {
    use proptest::prelude::*;

    use super::*;

    proptest! {
        /// Any single-key increment sequence produces the correct total.
        #[test]
        fn increment_total_matches_call_count(n in 1usize..200) {
            let rt = tokio::runtime::Runtime::new().unwrap();
            rt.block_on(async {
                let counter = SlidingWindowCounter::new();
                for _ in 0..n {
                    counter.increment("prop_key").await;
                }
                let total = counter.get("prop_key").await;
                prop_assert_eq!(total, n as u64);
                Ok(())
            })?;
        }

        /// Multiple keys stay independent under arbitrary access patterns.
        #[test]
        fn multiple_keys_isolated(hits_a in 1usize..50, hits_b in 1usize..50) {
            let rt = tokio::runtime::Runtime::new().unwrap();
            rt.block_on(async {
                let counter = SlidingWindowCounter::new();
                for _ in 0..hits_a { counter.increment("prop_a").await; }
                for _ in 0..hits_b { counter.increment("prop_b").await; }
                prop_assert_eq!(counter.get("prop_a").await, hits_a as u64);
                prop_assert_eq!(counter.get("prop_b").await, hits_b as u64);
                Ok(())
            })?;
        }
    }
}
