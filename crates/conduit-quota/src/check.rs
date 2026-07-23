use std::{future::Future, pin::Pin, sync::Arc};

use conduit_ir::error::QuotaError;

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
    /// Gateway correlation id (same as pipeline / `x-request-id`).
    pub request_id: String,
    /// Opaque identifier for the downstream API key.
    pub downstream_key_id: String,
    /// Requests-per-minute cap, if any.
    pub rate_limit_rpm: Option<u32>,
    /// The virtual model alias being requested.
    pub model_alias: String,
}

/// One upstream try within a gateway request (retry / fallback).
#[derive(Debug, Clone)]
pub struct QuotaAttemptRecord {
    pub attempt_no: u32,
    pub provider_id: Option<String>,
    pub provider_kind: Option<String>,
    pub model_id: Option<String>,
    /// `ok` | `error` | `partial` | …
    pub status: String,
    pub error_class: Option<String>,
    pub http_status: Option<u16>,
    pub duration_ms: Option<u64>,
    pub ttfb_ms: Option<u64>,
    /// `initial` | `retry` | …
    pub reason: Option<String>,
}

/// Input for [`QuotaEngine::record`] — one finished request's ledger entry.
///
/// Written to the durable usage ledger by the daemon. Always persisted for
/// observability (including zero-token success and terminal failures).
#[derive(Debug, Clone)]
pub struct QuotaRecordRequest {
    /// Stable request id (pipeline request correlation id).
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
    /// Outcome: `ok` | `error` | `cancelled` | `partial`.
    pub status: String,
    pub error_class: Option<String>,
    pub http_status: Option<u16>,
    pub finish_reason: Option<String>,
    pub duration_ms: Option<u64>,
    /// Time-to-first-byte in ms; null when no first byte was observed.
    pub ttfb_ms: Option<u64>,
    pub route_strategy: Option<String>,
    pub attempt_no: u32,
    pub attempt_count: u32,
    pub session_id: Option<String>,
    pub affinity_hit: Option<bool>,
    pub pool_id: Option<String>,
    pub selected_reason: Option<String>,
    /// Number of codec degradation warnings accumulated for this request
    /// ([`conduit_ir::loss::LossReport`] length). Details stay in logs, not the ledger.
    pub loss_count: u32,
    /// Client ingress wire protocol (`openai.chat` / `openai.responses` / `anthropic.messages`).
    pub wire_format: Option<String>,
    /// Per-try history (may be empty for single-shot success).
    pub attempts: Vec<QuotaAttemptRecord>,
}

impl QuotaRecordRequest {
    /// True when the request has any token or cost signal.
    ///
    /// Note: the engine **no longer skips** zero-consumption rows — the ledger
    /// records outcomes for success-rate / latency observability.
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
