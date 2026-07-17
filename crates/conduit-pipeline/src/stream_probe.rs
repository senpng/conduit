//! InstrumentedStream: wraps a provider stream to emit trace events and
//! record usage into the durable ledger (independent of traces).
//!
//! Stream audits preserve **real client-facing SSE frames** (OpenAI or
//! Anthropic wire), not only a reconstructed non-stream summary.

use std::{
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
};

use chrono::{DateTime, Utc};
use conduit_codec::{
    anthropic::stream::AnthropicStreamEncoder, openai::OpenAiCodec, WireCodec,
};
use conduit_ir::{
    canonical::{BlockDelta, CanonicalChunk, Usage},
    error::ProviderError,
    loss::LossReport,
    trace::{TraceEvent, TraceEventKind, WireFormat},
};
use conduit_quota::{engine::QuotaEngine, QuotaRecordRequest};
use conduit_trace::sink::TraceSink;
use futures::Stream;
use serde_json::json;
use tracing::warn;
use ulid::Ulid;

use super::{
    context::client_response_headers,
    egress::{compute_cost, ModelPricing},
};

pub type PricingFn = Arc<dyn Fn(&str, &str) -> Option<ModelPricing> + Send + Sync>;

// ---------------------------------------------------------------------------
// InstrumentedStream
// ---------------------------------------------------------------------------

/// Wraps a provider `BoxStream` to:
/// 1. Mark TTFB on the first chunk.
/// 2. Accumulate token usage, assistant text, and **wire SSE frames**.
/// 3. On stream completion (or drop), emit `UpstreamResponse` (with frames) +
///    `FinalUsage` under the shared `trace_id`, and write the usage ledger.
pub struct InstrumentedStream {
    inner: futures::stream::BoxStream<'static, Result<CanonicalChunk, ProviderError>>,
    finalized: bool,
    ttfb_at: Option<DateTime<Utc>>,
    started_at: DateTime<Utc>,
    usage_acc: Usage,
    /// Accumulated assistant text for summary audit.
    text_acc: String,
    tool_acc: Vec<serde_json::Value>,
    /// Exact SSE frames as would be delivered to the client.
    stream_frames: Vec<String>,
    /// Next StreamDelta sequence number (live tail + audit).
    stream_seq: u32,
    /// Stable response id used for wire chunk encoding.
    resp_id: String,
    sink: Arc<TraceSink>,
    pricing_fn: PricingFn,
    quota: Arc<dyn QuotaEngine>,
    downstream_key_id: Option<String>,
    alias: String,
    provider_id: String,
    provider_kind: String,
    model_id: String,
    loss_report: LossReport,
    trace_id: String,
    wire_format: WireFormat,
    /// Stateful Anthropic SSE encoder for client-facing audit frames.
    anthropic_encoder: Option<AnthropicStreamEncoder>,
}

impl InstrumentedStream {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        inner: futures::stream::BoxStream<'static, Result<CanonicalChunk, ProviderError>>,
        sink: Arc<TraceSink>,
        pricing_fn: PricingFn,
        quota: Arc<dyn QuotaEngine>,
        downstream_key_id: Option<String>,
        started_at: DateTime<Utc>,
        alias: String,
        provider_id: String,
        provider_kind: String,
        model_id: String,
        loss_report: LossReport,
        trace_id: String,
        wire_format: WireFormat,
    ) -> Self {
        let resp_id = Ulid::new().to_string();
        let anthropic_encoder = if wire_format == WireFormat::AnthropicMessages {
            Some(AnthropicStreamEncoder::new(resp_id.clone(), model_id.clone()))
        } else {
            None
        };
        Self {
            inner,
            finalized: false,
            ttfb_at: None,
            started_at,
            usage_acc: Usage::default(),
            text_acc: String::new(),
            tool_acc: Vec::new(),
            stream_frames: Vec::new(),
            stream_seq: 0,
            resp_id,
            sink,
            pricing_fn,
            quota,
            downstream_key_id,
            alias,
            provider_id,
            provider_kind,
            model_id,
            loss_report,
            trace_id,
            wire_format,
            anthropic_encoder,
        }
    }

    fn encode_wire_frames(&mut self, chunk: &CanonicalChunk) -> Vec<String> {
        match self.wire_format {
            WireFormat::OpenaiChat => OpenAiCodec::encode_chunk(chunk, &self.resp_id)
                .0
                .into_iter()
                .collect(),
            WireFormat::AnthropicMessages => {
                if let Some(enc) = self.anthropic_encoder.as_mut() {
                    enc.push(chunk)
                } else {
                    vec![]
                }
            }
            _ => OpenAiCodec::encode_chunk(chunk, &self.resp_id)
                .0
                .into_iter()
                .collect(),
        }
    }

    fn encode_stream_done(&mut self) -> Vec<String> {
        match self.wire_format {
            WireFormat::OpenaiChat => vec!["data: [DONE]\n\n".to_string()],
            WireFormat::AnthropicMessages => {
                if let Some(enc) = self.anthropic_encoder.as_mut() {
                    enc.finish()
                } else {
                    vec![]
                }
            }
            _ => vec!["data: [DONE]\n\n".to_string()],
        }
    }

    fn build_summary_json(&self) -> serde_json::Value {
        let message = if !self.tool_acc.is_empty() {
            json!({
                "role": "assistant",
                "content": if self.text_acc.is_empty() { serde_json::Value::Null } else { json!(self.text_acc) },
                "tool_calls": self.tool_acc,
            })
        } else {
            json!({
                "role": "assistant",
                "content": self.text_acc,
            })
        };
        json!({
            "object": "stream_summary",
            "wire_format": self.wire_format.as_str(),
            "stream": true,
            "frame_count": self.stream_frames.len(),
            "model": self.model_id,
            "choices": [{
                "index": 0,
                "message": message,
                "finish_reason": "stop",
            }],
            "usage": {
                "prompt_tokens": self.usage_acc.prompt_tokens,
                "completion_tokens": self.usage_acc.completion_tokens,
                "total_tokens": self.usage_acc.prompt_tokens + self.usage_acc.completion_tokens,
            }
        })
    }

    fn finalize_success(&mut self) {
        if self.finalized {
            return;
        }
        self.finalized = true;

        for done in self.encode_stream_done() {
            let seq = self.stream_seq;
            self.stream_seq = self.stream_seq.saturating_add(1);
            self.stream_frames.push(done.clone());
            let _ = self.sink.send(TraceEvent::with_trace_id(
                self.trace_id.clone(),
                TraceEventKind::StreamDelta {
                    seq,
                    frame: done,
                    text_delta: None,
                },
            ));
        }

        let latency_ms = (Utc::now() - self.started_at).num_milliseconds() as u64;
        let ttfb_ms = self
            .ttfb_at
            .map(|t| (t - self.started_at).num_milliseconds() as u64);

        let _ = self.sink.send(TraceEvent::with_trace_id(
            self.trace_id.clone(),
            TraceEventKind::UpstreamResponse {
                status: 200,
                latency_ms,
                ttfb_ms,
                response: Some(self.build_summary_json()),
                wire_format: Some(self.wire_format.to_string()),
                stream: true,
                stream_frames: Some(std::mem::take(&mut self.stream_frames)),
                response_headers: Some(client_response_headers(self.wire_format, true)),
            },
        ));

        let pf = &*self.pricing_fn;
        let cost_usd = compute_cost(
            &self.provider_kind,
            &self.model_id,
            &self.usage_acc,
            |pk, mid| pf(pk, mid),
        );

        let _ = self.sink.send(TraceEvent::with_trace_id(
            self.trace_id.clone(),
            TraceEventKind::FinalUsage {
                usage: self.usage_acc.clone(),
                cost_usd,
                loss_report: self.loss_report.clone(),
                downstream_key_id: self.downstream_key_id.clone(),
            },
        ));

        // Usage ledger — independent of the trace sink.
        let key_id = self
            .downstream_key_id
            .clone()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "_anonymous".into());
        let record = QuotaRecordRequest {
            request_id: self.trace_id.clone(),
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

    fn finalize_error(&mut self, kind: String, message: String) {
        if self.finalized {
            return;
        }
        self.finalized = true;

        let latency_ms = (Utc::now() - self.started_at).num_milliseconds() as u64;
        let ttfb_ms = self
            .ttfb_at
            .map(|t| (t - self.started_at).num_milliseconds() as u64);

        let partial = if self.stream_frames.is_empty()
            && self.text_acc.is_empty()
            && self.tool_acc.is_empty()
        {
            None
        } else {
            Some(self.build_summary_json())
        };

        let frames = if self.stream_frames.is_empty() {
            None
        } else {
            Some(std::mem::take(&mut self.stream_frames))
        };

        let _ = self.sink.send(TraceEvent::with_trace_id(
            self.trace_id.clone(),
            TraceEventKind::UpstreamResponse {
                status: 500,
                latency_ms,
                ttfb_ms,
                response: partial,
                wire_format: Some(self.wire_format.to_string()),
                stream: true,
                stream_frames: frames,
                response_headers: Some(client_response_headers(self.wire_format, true)),
            },
        ));
        let _ = self.sink.send(TraceEvent::with_trace_id(
            self.trace_id.clone(),
            TraceEventKind::Error { kind, message },
        ));
    }
}

impl Stream for InstrumentedStream {
    type Item = Result<CanonicalChunk, ProviderError>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        if self.finalized {
            return Poll::Ready(None);
        }

        match self.inner.as_mut().poll_next(cx) {
            Poll::Pending => Poll::Pending,

            Poll::Ready(Some(Ok(chunk))) => {
                if self.ttfb_at.is_none() {
                    self.ttfb_at = Some(Utc::now());
                }
                if let Some(ref usage) = chunk.usage {
                    self.usage_acc.merge(usage);
                }
                // Accumulate text for summary.
                if let Some(BlockDelta::TextDelta { text }) = &chunk.delta {
                    self.text_acc.push_str(text);
                }
                // Tool call start (name/id on chunk)
                if let (Some(id), Some(name)) = (&chunk.tool_use_id, &chunk.tool_name) {
                    self.tool_acc.push(json!({
                        "id": id,
                        "type": "function",
                        "function": {"name": name, "arguments": ""}
                    }));
                }
                if let Some(BlockDelta::InputJsonDelta { partial_json }) = &chunk.delta {
                    if let Some(last) = self.tool_acc.last_mut() {
                        if let Some(func) = last.get_mut("function") {
                            let args = func
                                .get("arguments")
                                .and_then(|a| a.as_str())
                                .unwrap_or("")
                                .to_string()
                                + partial_json;
                            func["arguments"] = json!(args);
                        }
                    }
                }

                // Capture + live-emit the real client-facing SSE frame(s) so
                // `trace tail` / admin SSE show stream content in real time.
                let text_delta = match &chunk.delta {
                    Some(BlockDelta::TextDelta { text }) if !text.is_empty() => Some(text.clone()),
                    _ => None,
                };
                for frame in self.encode_wire_frames(&chunk) {
                    let seq = self.stream_seq;
                    self.stream_seq = self.stream_seq.saturating_add(1);
                    self.stream_frames.push(frame.clone());
                    let _ = self.sink.send(TraceEvent::with_trace_id(
                        self.trace_id.clone(),
                        TraceEventKind::StreamDelta {
                            seq,
                            frame,
                            text_delta: text_delta.clone(),
                        },
                    ));
                }

                Poll::Ready(Some(Ok(chunk)))
            }

            Poll::Ready(Some(Err(e))) => {
                let kind = format!("{:?}", e);
                let message = e.to_string();
                self.finalize_error(kind, message);
                Poll::Ready(Some(Err(e)))
            }

            Poll::Ready(None) => {
                self.finalize_success();
                Poll::Ready(None)
            }
        }
    }
}

impl Drop for InstrumentedStream {
    fn drop(&mut self) {
        // If consumer stops early, still finalize audit trail with frames so far.
        if !self.finalized {
            self.finalize_success();
        }
    }
}
