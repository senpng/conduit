//! UsageTrackingStream: wraps a provider stream to accumulate usage and record cost.

use std::{
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
};

use chrono::{DateTime, Utc};
use conduit_ir::{
    canonical::{BlockDelta, CanonicalChunk, Usage},
    error::ProviderError,
};
use conduit_quota::{engine::QuotaEngine, QuotaRecordRequest};
use futures::Stream;
use tracing::warn;

use super::egress::{compute_cost, ModelPricing};

pub type PricingFn = Arc<dyn Fn(&str, &str) -> Option<ModelPricing> + Send + Sync>;

/// Wraps a provider stream; on completion records usage/cost to the quota ledger.
pub struct UsageTrackingStream {
    inner: futures::stream::BoxStream<'static, Result<CanonicalChunk, ProviderError>>,
    finalized: bool,
    usage_acc: Usage,
    pricing_fn: PricingFn,
    quota: Arc<dyn QuotaEngine>,
    request_id: String,
    downstream_key_id: Option<String>,
    alias: String,
    provider_id: String,
    provider_kind: String,
    model_id: String,
    /// Retained for API compatibility / future cost features.
    #[allow(dead_code)]
    started_at: DateTime<Utc>,
}

impl UsageTrackingStream {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        inner: futures::stream::BoxStream<'static, Result<CanonicalChunk, ProviderError>>,
        pricing_fn: PricingFn,
        quota: Arc<dyn QuotaEngine>,
        request_id: String,
        downstream_key_id: Option<String>,
        started_at: DateTime<Utc>,
        alias: String,
        provider_id: String,
        provider_kind: String,
        model_id: String,
    ) -> Self {
        Self {
            inner,
            finalized: false,
            usage_acc: Usage::default(),
            pricing_fn,
            quota,
            request_id,
            downstream_key_id,
            alias,
            provider_id,
            provider_kind,
            model_id,
            started_at,
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
        // Text deltas do not affect usage ledger; ignored here.
        let _ = chunk.delta.as_ref().and_then(|d| match d {
            BlockDelta::TextDelta { .. } => Some(()),
            _ => None,
        });
    }

    fn finalize(&mut self) {
        if self.finalized {
            return;
        }
        self.finalized = true;

        let pf = &*self.pricing_fn;
        let cost_usd =
            compute_cost(&self.provider_kind, &self.model_id, &self.usage_acc, |pk, mid| {
                pf(pk, mid)
            });

        let key_id = self
            .downstream_key_id
            .clone()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "_anonymous".into());
        let record = QuotaRecordRequest {
            request_id: self.request_id.clone(),
            downstream_key_id: key_id,
            alias: Some(self.alias.clone()),
            provider_id: Some(self.provider_id.clone()),
            provider_kind: Some(self.provider_kind.clone()),
            model_id: Some(self.model_id.clone()),
            prompt_tokens: self.usage_acc.prompt_tokens,
            completion_tokens: self.usage_acc.completion_tokens,
            total_tokens: self.usage_acc.total_tokens,
            reasoning_tokens: self.usage_acc.reasoning_tokens,
            cache_read_tokens: self.usage_acc.cache_read_tokens,
            cache_write_tokens: self.usage_acc.cache_write_tokens,
            cost_usd,
            stream: true,
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
                self.merge_chunk(&chunk);
                Poll::Ready(Some(Ok(chunk)))
            }
            Poll::Ready(Some(Err(e))) => {
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
            "req-stream-1".into(),
            Some("dk1".into()),
            Utc::now(),
            "gpt".into(),
            "openai".into(),
            "openai".into(),
            "gpt-4o".into(),
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
    }
}
