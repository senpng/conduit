use conduit_ir::trace::{TraceEvent, TraceEventKind};
use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// TraceRow — row written to the SQLite index
// ---------------------------------------------------------------------------

/// Flattened, indexable representation of a [`TraceEvent`] stored in SQLite.
#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct TraceIndexRow {
    pub id: String,
    /// Shared request audit id (all events of one gateway call share this).
    #[serde(default)]
    pub trace_id: String,
    /// Event type tag: request_received | routing_decided | ...
    #[serde(default)]
    pub kind: String,
    pub ts: String, // RFC 3339
    pub downstream_key_id: Option<String>,
    pub alias: String,
    pub provider_id: Option<String>,
    pub model_id: Option<String>,
    pub status_code: i64,
    pub latency_ms: i64,
    pub prompt_tokens: i64,
    pub completion_tokens: i64,
    pub reasoning_tokens: i64,
    pub cache_read_tokens: i64,
    pub cache_write_tokens: i64,
    pub cost_usd: f64,
    pub error_kind: Option<String>,
    /// Segment filename where the full event is stored.
    pub segment: String,
    /// Byte offset within that segment.
    pub offset: i64,
}

// ---------------------------------------------------------------------------
// Extraction helpers
// ---------------------------------------------------------------------------

/// Build a [`TraceIndexRow`] from a [`TraceEvent`] plus its log location.
pub fn event_to_index_row(event: &TraceEvent, segment: String, offset: u64) -> TraceIndexRow {
    let mut downstream_key_id: Option<String> = None;
    let mut alias = String::new();
    let mut provider_id: Option<String> = None;
    let mut model_id: Option<String> = None;
    let mut status_code: i64 = 0;
    let mut latency_ms: i64 = 0;
    let mut prompt_tokens: i64 = 0;
    let mut completion_tokens: i64 = 0;
    let mut reasoning_tokens: i64 = 0;
    let mut cache_read_tokens: i64 = 0;
    let mut cache_write_tokens: i64 = 0;
    let mut cost_usd: f64 = 0.0;
    let mut error_kind: Option<String> = None;

    match &event.kind {
        TraceEventKind::RequestReceived {
            downstream_key_id: dki,
            alias: a,
            ..
        } => {
            downstream_key_id = dki.clone();
            alias = a.clone();
        }
        TraceEventKind::RoutingDecided {
            provider_id: pid,
            model_id: mid,
            ..
        } => {
            provider_id = Some(pid.clone());
            model_id = Some(mid.clone());
        }
        TraceEventKind::StreamDelta { .. } => {
            // High-volume stream frames: no extra index columns.
        }
        TraceEventKind::UpstreamResponse {
            status,
            latency_ms: lms,
            ..
        } => {
            status_code = *status as i64;
            latency_ms = *lms as i64;
        }
        TraceEventKind::FinalUsage {
            usage,
            cost_usd: c,
            downstream_key_id: dki,
            ..
        } => {
            prompt_tokens = usage.prompt_tokens as i64;
            completion_tokens = usage.completion_tokens as i64;
            reasoning_tokens = usage.reasoning_tokens as i64;
            cache_read_tokens = usage.cache_read_tokens as i64;
            cache_write_tokens = usage.cache_write_tokens as i64;
            cost_usd = *c;
            downstream_key_id = dki.clone();
        }
        TraceEventKind::Error { kind, .. } => {
            error_kind = Some(kind.clone());
        }
        _ => {}
    }

    let trace_id = if event.trace_id.is_empty() {
        event.id.clone()
    } else {
        event.trace_id.clone()
    };

    TraceIndexRow {
        id: event.id.clone(),
        trace_id,
        kind: event.kind.type_name().to_string(),
        ts: event.ts.to_rfc3339(),
        downstream_key_id,
        alias,
        provider_id,
        model_id,
        status_code,
        latency_ms,
        prompt_tokens,
        completion_tokens,
        reasoning_tokens,
        cache_read_tokens,
        cache_write_tokens,
        cost_usd,
        error_kind,
        segment,
        offset: offset as i64,
    }
}
