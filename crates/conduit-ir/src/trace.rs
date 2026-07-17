use serde_json::Value;

use crate::{canonical::Usage, loss::LossReport};

/// A structured event emitted throughout the request lifecycle.
/// Events are appended to an in-memory log and flushed to the trace store after
/// the request completes.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct TraceEvent {
    /// ULID of this individual event (unique per event).
    pub id: String,
    /// Shared id for all events belonging to one gateway request (complete audit trail).
    #[serde(default)]
    pub trace_id: String,
    /// Wall-clock time the event was created.
    pub ts: chrono::DateTime<chrono::Utc>,
    pub kind: TraceEventKind,
}

impl TraceEvent {
    /// Construct a new event with a fresh id and empty `trace_id`.
    pub fn new(kind: TraceEventKind) -> Self {
        Self {
            id: ulid::Ulid::new().to_string(),
            trace_id: String::new(),
            ts: chrono::Utc::now(),
            kind,
        }
    }

    /// Construct an event belonging to an existing request audit trail.
    pub fn with_trace_id(trace_id: impl Into<String>, kind: TraceEventKind) -> Self {
        let tid = trace_id.into();
        Self {
            id: ulid::Ulid::new().to_string(),
            trace_id: tid,
            ts: chrono::Utc::now(),
            kind,
        }
    }
}

/// Downstream wire protocol for a gateway request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum WireFormat {
    /// OpenAI Chat Completions (`POST /v1/chat/completions`).
    OpenaiChat,
    /// Anthropic Messages (`POST /v1/messages`).
    AnthropicMessages,
}

impl WireFormat {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::OpenaiChat => "openai_chat",
            Self::AnthropicMessages => "anthropic_messages",
        }
    }
}

impl std::fmt::Display for WireFormat {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The structured payload of a `TraceEvent`.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
#[non_exhaustive]
pub enum TraceEventKind {
    /// The gateway received a new request from a downstream client.
    ///
    /// `request` is the **original wire body** as received (OpenAI/Anthropic JSON).
    /// `request_ir` is the decoded canonical IR for analysis / loss inspection.
    RequestReceived {
        /// Opaque downstream API key identifier (not the secret itself).
        downstream_key_id: Option<String>,
        /// The virtual model alias requested by the caller.
        alias: String,
        stream: bool,
        /// Original client wire body (preferred for forensic replay).
        /// Empty object when not available (legacy events).
        #[serde(default = "empty_object")]
        request: Value,
        /// Canonical IR JSON after decode (optional; present on new events).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        request_ir: Option<Value>,
        /// Wire protocol: `openai_chat` | `anthropic_messages`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        wire_format: Option<String>,
        /// Client request headers (secrets redacted). JSON object map.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        request_headers: Option<Value>,
    },

    /// The router selected a provider / model for this attempt.
    RoutingDecided {
        provider_id: String,
        model_id: String,
        /// Opaque upstream key identifier (not the secret itself).
        upstream_key_id: String,
        attempt_no: u32,
        /// LossReport from the codec encode step for this attempt.
        /// `None` until the attempt completes and the loss is attached.
        attempt_loss: Option<LossReport>,
    },

    /// One client-facing SSE frame emitted during a streaming response.
    ///
    /// Sent as the stream progresses so live `trace tail` / console SSE can show
    /// content in real time. Also persisted for complete stream audit.
    StreamDelta {
        /// 0-based sequence within this request's stream.
        seq: u32,
        /// Exact SSE frame text as delivered to the client (e.g. `"data: …\n\n"`).
        frame: String,
        /// Convenience extract of text delta when present.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        text_delta: Option<String>,
    },

    /// The upstream responded (whether success or HTTP error).
    ///
    /// * Non-stream: `response` is the exact wire JSON returned to the client.
    /// * Stream: `stream_frames` holds every SSE frame as sent; `response` is a
    ///   compact reconstructed summary for UI convenience.
    UpstreamResponse {
        status: u16,
        latency_ms: u64,
        /// Time-to-first-byte; `None` for non-streaming requests.
        ttfb_ms: Option<u64>,
        /// Wire response body (non-stream) or reconstructed summary (stream).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        response: Option<Value>,
        /// Wire protocol of the client-facing response.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        wire_format: Option<String>,
        /// Whether the client-facing response was SSE streaming.
        #[serde(default)]
        stream: bool,
        /// Exact SSE frames delivered to the client (stream only).
        /// Each entry is a full frame text, e.g. `"data: {...}\n\n"`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        stream_frames: Option<Vec<String>>,
        /// Client-facing response headers. JSON object map.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        response_headers: Option<Value>,
        /// Complete headers sent to the upstream provider. JSON object map.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        upstream_request_headers: Option<Value>,
        /// Complete headers returned by the upstream provider. JSON object map.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        upstream_response_headers: Option<Value>,
    },

    /// Token usage and cost summary for the completed request.
    FinalUsage {
        usage: Usage,
        cost_usd: f64,
        loss_report: LossReport,
        /// Downstream key to charge; `None` when the request was unauthenticated.
        downstream_key_id: Option<String>,
    },

    /// An error occurred at any stage.
    Error {
        /// Short machine-readable error kind (e.g. `"RateLimited"`, `"Timeout"`).
        kind: String,
        message: String,
    },
}

fn empty_object() -> Value {
    Value::Object(Default::default())
}

impl TraceEventKind {
    /// Stable type tag for indexing / filtering.
    pub fn type_name(&self) -> &'static str {
        match self {
            Self::RequestReceived { .. } => "request_received",
            Self::RoutingDecided { .. } => "routing_decided",
            Self::StreamDelta { .. } => "stream_delta",
            Self::UpstreamResponse { .. } => "upstream_response",
            Self::FinalUsage { .. } => "final_usage",
            Self::Error { .. } => "error",
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn event_has_ulid_and_timestamp() {
        let e = TraceEvent::new(TraceEventKind::Error {
            kind: "Timeout".into(),
            message: "upstream timed out".into(),
        });
        assert_eq!(e.id.len(), 26);
        let age = chrono::Utc::now() - e.ts;
        assert!(age.num_seconds() < 5);
    }

    #[test]
    fn request_received_includes_wire_and_ir() {
        let wire = serde_json::json!({
            "model": "gpt",
            "messages": [{"role": "user", "content": "hi"}],
            "stream": false
        });
        let ir = serde_json::json!({
            "alias": "gpt",
            "messages": [{"role": "user", "content": [{"type": "text", "text": "hi"}]}],
            "stream": false
        });
        let e = TraceEvent::with_trace_id(
            "trace-1",
            TraceEventKind::RequestReceived {
                downstream_key_id: Some("key_123".into()),
                alias: "gpt".into(),
                stream: false,
                request: wire.clone(),
                request_ir: Some(ir),
                wire_format: Some(WireFormat::OpenaiChat.to_string()),
                request_headers: Some(serde_json::json!({
                    "content-type": "application/json",
                    "authorization": "Bearer [REDACTED]"
                })),
            },
        );
        assert_eq!(e.trace_id, "trace-1");
        let json = serde_json::to_string(&e).unwrap();
        let back: TraceEvent = serde_json::from_str(&json).unwrap();
        match back.kind {
            TraceEventKind::RequestReceived {
                alias,
                request,
                request_ir,
                wire_format,
                request_headers,
                stream,
                ..
            } => {
                assert_eq!(alias, "gpt");
                assert!(!stream);
                assert_eq!(request["messages"][0]["content"], "hi");
                assert!(request_ir.is_some());
                assert_eq!(wire_format.as_deref(), Some("openai_chat"));
                assert_eq!(
                    request_headers.unwrap()["authorization"],
                    "Bearer [REDACTED]"
                );
            }
            _ => panic!("unexpected kind"),
        }
    }

    #[test]
    fn legacy_request_received_deserializes_without_new_fields() {
        let json = r#"{
            "id": "01TEST",
            "ts": "2026-01-01T00:00:00Z",
            "kind": {
                "type": "request_received",
                "downstream_key_id": null,
                "alias": "gpt-4o",
                "stream": true
            }
        }"#;
        let back: TraceEvent = serde_json::from_str(json).unwrap();
        match back.kind {
            TraceEventKind::RequestReceived {
                alias,
                request,
                request_ir,
                wire_format,
                ..
            } => {
                assert_eq!(alias, "gpt-4o");
                assert!(request.as_object().unwrap().is_empty());
                assert!(request_ir.is_none());
                assert!(wire_format.is_none());
            }
            _ => panic!("unexpected kind"),
        }
    }

    #[test]
    fn final_usage_roundtrip() {
        let usage = Usage {
            prompt_tokens: 100,
            completion_tokens: 50,
            total_tokens: 150,
            reasoning_tokens: 10,
            cache_read_tokens: 5,
            cache_write_tokens: 0,
        };
        let mut loss = LossReport::default();
        loss.add("tool_choice", "AnyOf", "Required", "not supported");
        let e = TraceEvent::new(TraceEventKind::FinalUsage {
            usage: usage.clone(),
            cost_usd: 0.0012,
            loss_report: loss,
            downstream_key_id: Some("dk-test".into()),
        });
        let json = serde_json::to_string(&e).unwrap();
        let back: TraceEvent = serde_json::from_str(&json).unwrap();
        match back.kind {
            TraceEventKind::FinalUsage {
                usage: u,
                cost_usd,
                loss_report,
                ..
            } => {
                assert_eq!(u.prompt_tokens, 100);
                assert_eq!(u.reasoning_tokens, 10);
                assert!((cost_usd - 0.0012).abs() < 1e-9);
                assert_eq!(loss_report.len(), 1);
            }
            _ => panic!("unexpected kind"),
        }
    }

    #[test]
    fn upstream_response_with_stream_frames() {
        let e = TraceEvent::with_trace_id(
            "t1",
            TraceEventKind::UpstreamResponse {
                status: 200,
                latency_ms: 100,
                ttfb_ms: Some(12),
                response: Some(serde_json::json!({"object": "stream_summary", "frame_count": 2})),
                wire_format: Some(WireFormat::OpenaiChat.to_string()),
                stream: true,
                stream_frames: Some(vec![
                    "data: {\"choices\":[{\"delta\":{\"content\":\"hi\"}}]}\n\n".into(),
                    "data: [DONE]\n\n".into(),
                ]),
                response_headers: Some(serde_json::json!({
                    "content-type": "text/event-stream",
                    "cache-control": "no-cache"
                })),
            },
        );
        let v: Value = serde_json::to_value(&e).unwrap();
        assert_eq!(v["kind"]["type"], "upstream_response");
        assert_eq!(v["kind"]["stream"], true);
        assert_eq!(v["kind"]["stream_frames"].as_array().unwrap().len(), 2);
        assert_eq!(v["kind"]["wire_format"], "openai_chat");
        assert_eq!(v["trace_id"], "t1");
    }

    #[test]
    fn legacy_upstream_response_deserializes() {
        let json = r#"{
            "id": "01X",
            "ts": "2026-01-01T00:00:00Z",
            "kind": {
                "type": "upstream_response",
                "status": 200,
                "latency_ms": 10,
                "ttfb_ms": null,
                "response": {"choices": [{"message": {"content": "hello"}}]}
            }
        }"#;
        let back: TraceEvent = serde_json::from_str(json).unwrap();
        match back.kind {
            TraceEventKind::UpstreamResponse {
                stream,
                stream_frames,
                wire_format,
                response,
                ..
            } => {
                assert!(!stream);
                assert!(stream_frames.is_none());
                assert!(wire_format.is_none());
                assert_eq!(
                    response.unwrap()["choices"][0]["message"]["content"],
                    "hello"
                );
            }
            _ => panic!("unexpected kind"),
        }
    }
}
