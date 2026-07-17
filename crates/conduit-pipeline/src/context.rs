//! PipelineContext: all mutable state threaded through the L1-L7 pipeline.

use std::{
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

use chrono::{DateTime, Utc};
use conduit_ir::{
    canonical::{CanonicalChatRequest, Usage},
    loss::LossReport,
    trace::{TraceEventKind, WireFormat},
};
use conduit_router::table::RoutingTable;
use serde_json::Value;
use ulid::Ulid;

/// Holds the resolved key material for a single upstream call.
#[derive(Clone)]
pub struct ResolvedProvider {
    pub provider_id: String,
    pub model_id: String,
    pub upstream_key_id: String,
    pub provider_kind: String,
    pub base_url: Option<String>,
    pub attempt_no: u32,
}

/// Client-facing wire body + protocol for complete audit.
#[derive(Debug, Clone)]
pub struct IngressWire {
    pub format: WireFormat,
    /// Original JSON body as received from the client (before IR decode).
    pub body: Value,
    /// Original client request headers. JSON object map.
    pub headers: Value,
}

/// Mutable accumulator that flows through the entire pipeline.
pub struct PipelineContext {
    pub trace_id: String,
    pub started_at: DateTime<Utc>,
    pub request: CanonicalChatRequest,
    pub downstream_key_id: Option<String>,
    pub routing_table: Arc<RoutingTable>,
    pub resolved: Option<ResolvedProvider>,
    pub usage: Usage,
    pub loss_report: LossReport,
    pub events: Vec<TraceEventKind>,
    pub attempt_no: u32,
    /// Original client wire (for faithful request/response audit).
    pub ingress_wire: Option<IngressWire>,
    /// Fixed for the whole request so Weighted LB retries keep a stable target order.
    pub routing_seed: u64,
}

impl PipelineContext {
    pub fn new(
        request: CanonicalChatRequest,
        downstream_key_id: Option<String>,
        routing_table: Arc<RoutingTable>,
    ) -> Self {
        let routing_seed = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0);
        Self {
            trace_id: Ulid::new().to_string(),
            started_at: Utc::now(),
            request,
            downstream_key_id,
            routing_table,
            resolved: None,
            usage: Usage::default(),
            loss_report: LossReport::default(),
            events: Vec::new(),
            attempt_no: 0,
            ingress_wire: None,
            routing_seed,
        }
    }

    pub fn with_ingress_wire(mut self, wire: IngressWire) -> Self {
        self.ingress_wire = Some(wire);
        self
    }

    pub fn latency_ms(&self) -> u64 {
        (Utc::now() - self.started_at).num_milliseconds() as u64
    }

    pub fn push_event(&mut self, kind: TraceEventKind) {
        self.events.push(kind);
    }

    pub fn merge_usage(&mut self, delta: &Usage) {
        self.usage.prompt_tokens += delta.prompt_tokens;
        self.usage.completion_tokens += delta.completion_tokens;
        self.usage.reasoning_tokens += delta.reasoning_tokens;
        self.usage.cache_read_tokens += delta.cache_read_tokens;
        self.usage.cache_write_tokens += delta.cache_write_tokens;
        // Prefer explicit total; otherwise keep prompt+completion consistent.
        if delta.total_tokens > 0 {
            self.usage.total_tokens += delta.total_tokens;
        } else {
            self.usage.total_tokens = self.usage.prompt_tokens + self.usage.completion_tokens;
        }
    }
}

/// Headers the gateway sets on the client-facing response (for audit).
pub fn client_response_headers(_fmt: WireFormat, stream: bool) -> Value {
    if stream {
        serde_json::json!({
            "content-type": "text/event-stream",
            "cache-control": "no-cache",
            "x-accel-buffering": "no",
        })
    } else {
        serde_json::json!({
            "content-type": "application/json",
        })
    }
}
