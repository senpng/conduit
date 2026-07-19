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
    /// Opaque identifier for the downstream API key.
    pub downstream_key_id: String,
    /// Requests-per-minute cap, if any.
    pub rate_limit_rpm: Option<u32>,
    /// The virtual model alias being requested.
    pub model_alias: String,
}

/// Input for [`QuotaEngine::record`] — one completed request's consumption.
///
/// Written to the durable usage ledger by the daemon.
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
}

impl QuotaRecordRequest {
    /// True when the request has any token or cost signal worth recording.
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
