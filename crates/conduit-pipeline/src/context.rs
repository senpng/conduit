//! PipelineContext: mutable state threaded through the L1–L7 pipeline.

use std::{
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

use chrono::{DateTime, Utc};
use conduit_ir::{
    canonical::{CanonicalChatRequest, Usage},
    loss::LossReport,
    wire_format::WireFormat,
};
use conduit_router::table::RoutingTable;
use serde_json::Value;
use ulid::Ulid;

/// Holds the resolved target for a single upstream call.
///
/// Credentials are looked up by [`provider_id`](Self::provider_id) (secret is
/// bound on the provider, not the route).
#[derive(Clone)]
pub struct ResolvedProvider {
    pub provider_id: String,
    pub model_id: String,
    pub provider_kind: String,
    pub base_url: Option<String>,
    pub request_overrides: serde_json::Map<String, Value>,
    pub attempt_no: u32,
}

/// Client-facing wire protocol for a gateway request.
#[derive(Debug, Clone)]
pub struct IngressWire {
    pub format: WireFormat,
}

/// Mutable accumulator that flows through the entire pipeline.
pub struct PipelineContext {
    /// Correlation id shared with ingress (`CanonicalChatRequest.id`), usage
    /// ledger, logs, and the `x-request-id` response header.
    pub request_id: String,
    pub started_at: DateTime<Utc>,
    pub request: CanonicalChatRequest,
    pub downstream_key_id: Option<String>,
    pub routing_table: Arc<RoutingTable>,
    pub resolved: Option<ResolvedProvider>,
    pub usage: Usage,
    pub loss_report: LossReport,
    pub attempt_no: u32,
    pub ingress_wire: Option<IngressWire>,
    pub routing_seed: u64,
    /// Session identity for affinity (empty / None → no pin).
    pub session_id: Option<String>,
    /// Client headers forwarded for session extraction / upstream.
    pub client_headers: Vec<(String, String)>,
}

impl PipelineContext {
    pub fn new(
        request: CanonicalChatRequest,
        downstream_key_id: Option<String>,
        routing_table: Arc<RoutingTable>,
        wire_format: WireFormat,
    ) -> Self {
        let routing_seed = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0);

        // One correlation id for the whole gateway request: reuse the ingress
        // ULID on `CanonicalChatRequest.id` so logs, usage ledger, and the
        // `x-request-id` response header all share the same value. Only mint a
        // fresh ULID when callers (tests / compact path) leave `id` empty.
        let request_id = if request.id.is_empty() {
            Ulid::new().to_string()
        } else {
            request.id.clone()
        };

        Self {
            request_id,
            started_at: Utc::now(),
            request,
            downstream_key_id,
            routing_table,
            resolved: None,
            usage: Usage::default(),
            loss_report: LossReport::default(),
            attempt_no: 0,
            ingress_wire: Some(IngressWire {
                format: wire_format,
            }),
            routing_seed,
            session_id: None,
            client_headers: Vec::new(),
        }
    }

    pub fn with_client_headers(mut self, headers: Vec<(String, String)>) -> Self {
        self.client_headers = headers;
        self
    }

    pub fn with_ingress_wire(mut self, wire: IngressWire) -> Self {
        self.ingress_wire = Some(wire);
        self
    }

    pub fn latency_ms(&self) -> u64 {
        (Utc::now() - self.started_at).num_milliseconds() as u64
    }

    pub fn merge_usage(&mut self, delta: &Usage) {
        self.usage.prompt_tokens += delta.prompt_tokens;
        self.usage.completion_tokens += delta.completion_tokens;
        self.usage.reasoning_tokens += delta.reasoning_tokens;
        self.usage.cache_read_tokens += delta.cache_read_tokens;
        self.usage.cache_write_tokens += delta.cache_write_tokens;
        if delta.total_tokens > 0 {
            self.usage.total_tokens += delta.total_tokens;
        } else {
            self.usage.total_tokens = self.usage.prompt_tokens + self.usage.completion_tokens;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use conduit_ir::canonical::CanonicalChatRequest;
    use conduit_router::table::RoutingTable;

    #[test]
    fn request_id_reuses_ingress_id() {
        let mut req = CanonicalChatRequest::new("gpt", vec![]);
        req.id = "01TESTINGRESSID0000000000000".into();
        let table = Arc::new(RoutingTable::new([]));
        let ctx = PipelineContext::new(req, None, table, WireFormat::OpenaiChat);
        assert_eq!(ctx.request_id, "01TESTINGRESSID0000000000000");
        assert_eq!(ctx.request_id, ctx.request.id);
    }

    #[test]
    fn request_id_mints_when_ingress_empty() {
        let mut req = CanonicalChatRequest::new("gpt", vec![]);
        req.id.clear();
        let table = Arc::new(RoutingTable::new([]));
        let ctx = PipelineContext::new(req, None, table, WireFormat::OpenaiChat);
        assert!(!ctx.request_id.is_empty());
    }
}
