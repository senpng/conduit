//! UsageTrackingStream: wraps a provider stream to accumulate usage and record cost.

use std::{
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
};

use chrono::{DateTime, Utc};
use conduit_ir::{
    canonical::{BlockDelta, CanonicalChunk, FinishReason, Usage},
    error::ProviderError,
};
use conduit_quota::{engine::QuotaEngine, QuotaAttemptRecord, QuotaRecordRequest};
use futures::Stream;
use tracing::warn;

use super::egress::{compute_cost, ModelPricing};

pub type PricingFn = Arc<dyn Fn(&str, &str) -> Option<ModelPricing> + Send + Sync>;

/// Metadata captured at stream start for the request ledger.
#[derive(Debug, Clone)]
pub struct StreamRecordMeta {
    pub request_id: String,
    pub downstream_key_id: Option<String>,
    pub alias: String,
    pub provider_id: String,
    pub provider_kind: String,
    pub model_id: String,
    /// Request start (gateway accept) — main-row duration / client TTFB.
    pub started_at: DateTime<Utc>,
    /// This upstream try's start — final attempt row duration / attempt TTFB.
    pub attempt_started_at: DateTime<Utc>,
    pub route_strategy: Option<String>,
    pub attempt_no: u32,
    pub attempt_count: u32,
    pub session_id: Option<String>,
    pub affinity_hit: Option<bool>,
    pub pool_id: Option<String>,
    pub selected_reason: Option<String>,
    /// Failed (or prior) attempts before this stream was opened.
    pub prior_attempts: Vec<QuotaAttemptRecord>,
}

/// Wraps a provider stream; on completion records usage/cost to the quota ledger.
pub struct UsageTrackingStream {
    inner: futures::stream::BoxStream<'static, Result<CanonicalChunk, ProviderError>>,
    finalized: bool,
    usage_acc: Usage,
    pricing_fn: PricingFn,
    quota: Arc<dyn QuotaEngine>,
    meta: StreamRecordMeta,
    /// Request-level TTFB (first Ok chunk vs `started_at`) for the main ledger row.
    ttfb_ms: Option<u64>,
    /// Attempt-local TTFB (first Ok chunk vs `attempt_started_at`) for the final try row.
    attempt_ttfb_ms: Option<u64>,
    finish_reason: Option<String>,
    /// Last terminal stream error, if any.
    terminal_error: Option<ProviderError>,
}

impl UsageTrackingStream {
    pub fn new(
        inner: futures::stream::BoxStream<'static, Result<CanonicalChunk, ProviderError>>,
        pricing_fn: PricingFn,
        quota: Arc<dyn QuotaEngine>,
        meta: StreamRecordMeta,
    ) -> Self {
        Self {
            inner,
            finalized: false,
            usage_acc: Usage::default(),
            pricing_fn,
            quota,
            meta,
            ttfb_ms: None,
            attempt_ttfb_ms: None,
            finish_reason: None,
            terminal_error: None,
        }
    }

    fn merge_chunk(&mut self, chunk: &CanonicalChunk) {
        if let Some(u) = &chunk.usage {
            self.usage_acc.prompt_tokens += u.prompt_tokens;
            self.usage_acc.completion_tokens += u.completion_tokens;
            self.usage_acc.reasoning_tokens += u.reasoning_tokens;
            self.usage_acc.cache_read_tokens += u.cache_read_tokens;
            self.usage_acc.cache_write_tokens += u.cache_write_tokens;
            if u.total_tokens > 0 {
                self.usage_acc.total_tokens += u.total_tokens;
            } else {
                self.usage_acc.total_tokens =
                    self.usage_acc.prompt_tokens + self.usage_acc.completion_tokens;
            }
        }
        if let Some(fr) = &chunk.finish_reason {
            self.finish_reason = Some(finish_reason_str(fr));
        }
        // Text deltas do not affect usage ledger; ignored here.
        let _ = chunk.delta.as_ref().and_then(|d| match d {
            BlockDelta::TextDelta { .. } => Some(()),
            _ => None,
        });
    }

    fn stamp_ttfb(&mut self) {
        let now = Utc::now();
        // Main-row / client TTFB: from request accept.
        if self.ttfb_ms.is_none() {
            let ms = (now - self.meta.started_at).num_milliseconds().max(0) as u64;
            self.ttfb_ms = Some(ms);
        }
        // Per-try TTFB: from this upstream attempt's start only.
        if self.attempt_ttfb_ms.is_none() {
            let ms = (now - self.meta.attempt_started_at)
                .num_milliseconds()
                .max(0) as u64;
            self.attempt_ttfb_ms = Some(ms);
        }
    }

    fn finalize(&mut self) {
        if self.finalized {
            return;
        }
        self.finalized = true;

        let pf = &*self.pricing_fn;
        let cost_usd =
            compute_cost(&self.meta.provider_kind, &self.meta.model_id, &self.usage_acc, |pk, mid| {
                pf(pk, mid)
            });

        let now = Utc::now();
        // Main ledger row: end-to-end from request accept.
        let duration_ms = (now - self.meta.started_at).num_milliseconds().max(0) as u64;
        // Final attempt row: only this try (not prior retries).
        let attempt_duration_ms = (now - self.meta.attempt_started_at)
            .num_milliseconds()
            .max(0) as u64;

        let (status, error_class, http_status) = match &self.terminal_error {
            None => ("ok".to_string(), None, None),
            Some(e) => {
                let has_tokens = self.usage_acc.total_tokens > 0
                    || self.usage_acc.prompt_tokens > 0
                    || self.usage_acc.completion_tokens > 0;
                let status = if has_tokens {
                    "partial".to_string()
                } else {
                    "error".to_string()
                };
                (
                    status,
                    Some(provider_error_class(e)),
                    e.http_status_hint(),
                )
            }
        };

        let key_id = self
            .meta
            .downstream_key_id
            .clone()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "_anonymous".into());

        let mut attempts = self.meta.prior_attempts.clone();
        attempts.push(QuotaAttemptRecord {
            attempt_no: self.meta.attempt_no,
            provider_id: Some(self.meta.provider_id.clone()),
            provider_kind: Some(self.meta.provider_kind.clone()),
            model_id: Some(self.meta.model_id.clone()),
            status: status.clone(),
            error_class: error_class.clone(),
            http_status,
            duration_ms: Some(attempt_duration_ms),
            ttfb_ms: self.attempt_ttfb_ms,
            reason: Some(if self.meta.attempt_no == 0 {
                "initial".into()
            } else {
                "retry".into()
            }),
        });

        let record = QuotaRecordRequest {
            request_id: self.meta.request_id.clone(),
            downstream_key_id: key_id,
            alias: Some(self.meta.alias.clone()),
            provider_id: Some(self.meta.provider_id.clone()),
            provider_kind: Some(self.meta.provider_kind.clone()),
            model_id: Some(self.meta.model_id.clone()),
            prompt_tokens: self.usage_acc.prompt_tokens,
            completion_tokens: self.usage_acc.completion_tokens,
            total_tokens: self.usage_acc.total_tokens,
            reasoning_tokens: self.usage_acc.reasoning_tokens,
            cache_read_tokens: self.usage_acc.cache_read_tokens,
            cache_write_tokens: self.usage_acc.cache_write_tokens,
            cost_usd,
            stream: true,
            status,
            error_class,
            http_status,
            finish_reason: self.finish_reason.clone(),
            duration_ms: Some(duration_ms),
            ttfb_ms: self.ttfb_ms,
            route_strategy: self.meta.route_strategy.clone(),
            attempt_no: self.meta.attempt_no,
            attempt_count: self.meta.attempt_count.max(1),
            session_id: self.meta.session_id.clone(),
            affinity_hit: self.meta.affinity_hit,
            pool_id: self.meta.pool_id.clone(),
            selected_reason: self.meta.selected_reason.clone(),
            attempts,
        };
        let quota = self.quota.clone();
        tokio::spawn(async move {
            if let Err(e) = quota.record(&record).await {
                warn!(error = %e, "stream usage record failed");
            }
        });
    }
}

impl Stream for UsageTrackingStream {
    type Item = Result<CanonicalChunk, ProviderError>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        match Pin::new(&mut self.inner).poll_next(cx) {
            Poll::Ready(Some(Ok(chunk))) => {
                self.stamp_ttfb();
                self.merge_chunk(&chunk);
                Poll::Ready(Some(Ok(chunk)))
            }
            Poll::Ready(Some(Err(e))) => {
                // Clone error for ledger; still surface original to client.
                self.terminal_error = Some(clone_provider_error(&e));
                self.finalize();
                Poll::Ready(Some(Err(e)))
            }
            Poll::Ready(None) => {
                self.finalize();
                Poll::Ready(None)
            }
            Poll::Pending => Poll::Pending,
        }
    }
}

impl Drop for UsageTrackingStream {
    fn drop(&mut self) {
        if !self.finalized {
            self.finalize();
        }
    }
}

pub(crate) fn provider_error_class(e: &ProviderError) -> String {
    match e {
        ProviderError::RateLimited(_) => "rate_limited".into(),
        ProviderError::Unauthorized(_) => "unauthorized".into(),
        ProviderError::InvalidRequest(_) => "invalid_request".into(),
        ProviderError::Upstream5xx(_) => "upstream_5xx".into(),
        ProviderError::Network(_) => "network".into(),
        ProviderError::Serialization(_) => "serialization".into(),
        ProviderError::Timeout => "timeout".into(),
        ProviderError::ContextLengthExceeded => "context_length_exceeded".into(),
        _ => "upstream_error".into(),
    }
}

pub(crate) fn finish_reason_str(fr: &FinishReason) -> String {
    match fr {
        FinishReason::Stop => "stop".into(),
        FinishReason::Length => "length".into(),
        FinishReason::ToolCalls => "tool_calls".into(),
        FinishReason::ContentFilter => "content_filter".into(),
        FinishReason::Other(s) => s.clone(),
        _ => "other".into(),
    }
}

fn clone_provider_error(e: &ProviderError) -> ProviderError {
    match e {
        ProviderError::RateLimited(s) => ProviderError::RateLimited(s.clone()),
        ProviderError::Unauthorized(s) => ProviderError::Unauthorized(s.clone()),
        ProviderError::InvalidRequest(s) => ProviderError::InvalidRequest(s.clone()),
        ProviderError::Upstream5xx(s) => ProviderError::Upstream5xx(s.clone()),
        ProviderError::Network(s) => ProviderError::Network(s.clone()),
        ProviderError::Serialization(s) => ProviderError::Serialization(s.clone()),
        ProviderError::Timeout => ProviderError::Timeout,
        ProviderError::ContextLengthExceeded => ProviderError::ContextLengthExceeded,
        // non_exhaustive: best-effort string preserve for ledger status path
        other => ProviderError::Network(other.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use async_trait::async_trait;
    use futures::stream::{self, StreamExt};

    use super::*;

    struct CapturingQuota {
        recorded: Arc<std::sync::Mutex<Vec<QuotaRecordRequest>>>,
    }

    #[async_trait]
    impl QuotaEngine for CapturingQuota {
        async fn check(
            &self,
            _: &conduit_quota::check::QuotaCheckRequest,
        ) -> Result<(), conduit_ir::error::QuotaError> {
            Ok(())
        }
        async fn record(
            &self,
            req: &conduit_quota::check::QuotaRecordRequest,
        ) -> Result<(), conduit_ir::error::QuotaError> {
            self.recorded.lock().unwrap().push(req.clone());
            Ok(())
        }
    }

    fn meta(request_id: &str) -> StreamRecordMeta {
        let now = Utc::now();
        StreamRecordMeta {
            request_id: request_id.into(),
            downstream_key_id: Some("dk1".into()),
            alias: "gpt".into(),
            provider_id: "openai".into(),
            provider_kind: "openai".into(),
            model_id: "gpt-4o".into(),
            started_at: now,
            attempt_started_at: now,
            route_strategy: Some("fixed".into()),
            attempt_no: 0,
            attempt_count: 1,
            session_id: None,
            affinity_hit: None,
            pool_id: None,
            selected_reason: Some("fixed".into()),
            prior_attempts: Vec::new(),
        }
    }

    #[tokio::test]
    async fn stream_completion_records_usage_via_quota_engine() {
        let recorded = Arc::new(std::sync::Mutex::new(Vec::new()));
        let quota = Arc::new(CapturingQuota {
            recorded: recorded.clone(),
        });

        let chunk = CanonicalChunk {
            usage: Some(Usage {
                prompt_tokens: 10,
                completion_tokens: 5,
                total_tokens: 15,
                reasoning_tokens: 0,
                cache_read_tokens: 0,
                cache_write_tokens: 0,
            }),
            ..CanonicalChunk::text_delta("hi")
        };
        let inner = stream::iter(vec![Ok(chunk)]);
        let mut stream = UsageTrackingStream::new(
            Box::pin(inner),
            Arc::new(|_, _| {
                Some(ModelPricing {
                    input_per_mtok: 1.0,
                    output_per_mtok: 2.0,
                    cache_read_per_mtok: None,
                    cache_write_per_mtok: None,
                    reasoning_per_mtok: None,
                })
            }),
            quota,
            meta("req-stream-1"),
        );

        while stream.next().await.is_some() {}
        // Allow spawned record task to finish.
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        let rows = recorded.lock().unwrap().clone();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].request_id, "req-stream-1");
        assert_eq!(rows[0].downstream_key_id, "dk1");
        assert_eq!(rows[0].prompt_tokens, 10);
        assert_eq!(rows[0].completion_tokens, 5);
        assert!(rows[0].stream);
        assert!(rows[0].cost_usd > 0.0);
        assert_eq!(rows[0].status, "ok");
        assert!(rows[0].ttfb_ms.is_some());
        assert!(rows[0].duration_ms.is_some());
        assert!(rows[0].duration_ms.unwrap() >= rows[0].ttfb_ms.unwrap());
    }

    #[tokio::test]
    async fn stream_first_chunk_stamps_ttfb_then_finalize_duration() {
        let recorded = Arc::new(std::sync::Mutex::new(Vec::new()));
        let quota = Arc::new(CapturingQuota {
            recorded: recorded.clone(),
        });

        let started = Utc::now() - chrono::Duration::milliseconds(50);
        let mut m = meta("req-ttfb");
        m.started_at = started;
        m.attempt_started_at = started;

        let c1 = CanonicalChunk::text_delta("a");
        let c2 = CanonicalChunk {
            usage: Some(Usage {
                prompt_tokens: 1,
                completion_tokens: 1,
                total_tokens: 2,
                ..Usage::default()
            }),
            ..CanonicalChunk::text_delta("b")
        };
        let mut stream = UsageTrackingStream::new(
            Box::pin(stream::iter(vec![Ok(c1), Ok(c2)])),
            Arc::new(|_, _| None),
            quota,
            m,
        );
        while stream.next().await.is_some() {}
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        let rows = recorded.lock().unwrap().clone();
        assert_eq!(rows.len(), 1);
        let ttfb = rows[0].ttfb_ms.expect("ttfb");
        let duration = rows[0].duration_ms.expect("duration");
        assert!(ttfb >= 50, "ttfb={ttfb}");
        assert!(duration >= ttfb, "duration={duration} ttfb={ttfb}");
    }

    #[tokio::test]
    async fn stream_error_records_error_status() {
        let recorded = Arc::new(std::sync::Mutex::new(Vec::new()));
        let quota = Arc::new(CapturingQuota {
            recorded: recorded.clone(),
        });
        let mut stream = UsageTrackingStream::new(
            Box::pin(stream::iter(vec![Err(ProviderError::Timeout)])),
            Arc::new(|_, _| None),
            quota,
            meta("req-err-stream"),
        );
        let _ = stream.next().await;
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        let rows = recorded.lock().unwrap().clone();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].status, "error");
        assert_eq!(rows[0].error_class.as_deref(), Some("timeout"));
        assert!(rows[0].ttfb_ms.is_none());
    }

    #[tokio::test]
    async fn stream_includes_prior_attempts() {
        let recorded = Arc::new(std::sync::Mutex::new(Vec::new()));
        let quota = Arc::new(CapturingQuota {
            recorded: recorded.clone(),
        });
        // Request accepted 250ms ago (includes failed try); this attempt just started.
        let request_started = Utc::now() - chrono::Duration::milliseconds(250);
        let attempt_started = Utc::now() - chrono::Duration::milliseconds(15);
        let mut m = meta("req-multi");
        m.started_at = request_started;
        m.attempt_started_at = attempt_started;
        m.attempt_no = 1;
        m.attempt_count = 2;
        m.prior_attempts = vec![QuotaAttemptRecord {
            attempt_no: 0,
            provider_id: Some("p0".into()),
            provider_kind: Some("openai".into()),
            model_id: Some("gpt-4o".into()),
            status: "error".into(),
            error_class: Some("rate_limited".into()),
            http_status: Some(429),
            duration_ms: Some(200),
            ttfb_ms: None,
            reason: Some("initial".into()),
        }];
        let chunk = CanonicalChunk {
            usage: Some(Usage {
                prompt_tokens: 1,
                completion_tokens: 1,
                total_tokens: 2,
                ..Usage::default()
            }),
            ..CanonicalChunk::text_delta("ok")
        };
        let mut stream = UsageTrackingStream::new(
            Box::pin(stream::iter(vec![Ok(chunk)])),
            Arc::new(|_, _| None),
            quota,
            m,
        );
        while stream.next().await.is_some() {}
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        let rows = recorded.lock().unwrap().clone();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].attempts.len(), 2);
        assert_eq!(rows[0].attempts[0].provider_id.as_deref(), Some("p0"));
        assert_eq!(rows[0].attempts[1].status, "ok");
        assert_eq!(rows[0].attempt_count, 2);

        let main_duration = rows[0].duration_ms.expect("main duration");
        let final_attempt = &rows[0].attempts[1];
        let attempt_duration = final_attempt.duration_ms.expect("attempt duration");
        // Main row spans the whole request (prior try + current).
        assert!(
            main_duration >= 250,
            "main duration must cover request start, got {main_duration}"
        );
        // Final attempt is attempt-local — not prior+current.
        assert!(
            attempt_duration < 100,
            "final attempt duration must be attempt-local, got {attempt_duration}"
        );
        assert!(
            attempt_duration < main_duration,
            "attempt {attempt_duration} must be < main {main_duration}"
        );
        // Attempt TTFB is also relative to attempt start, not request start.
        let attempt_ttfb = final_attempt.ttfb_ms.expect("attempt ttfb");
        let main_ttfb = rows[0].ttfb_ms.expect("main ttfb");
        assert!(
            attempt_ttfb <= main_ttfb,
            "attempt ttfb {attempt_ttfb} should be ≤ main ttfb {main_ttfb}"
        );
        assert!(
            attempt_ttfb < 100,
            "attempt ttfb must be attempt-local, got {attempt_ttfb}"
        );
    }
}
