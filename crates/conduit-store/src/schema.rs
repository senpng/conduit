use serde::{Deserialize, Serialize};

// ── Provider ─────────────────────────────────────────────────────────────────

/// Raw DB row for a provider record.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderRow {
    pub id: String,
    /// Human-readable name (e.g. "My OpenAI Account").
    pub name: String,
    /// Provider kind discriminant: `"openai"`, `"anthropic"`, etc.
    pub kind: String,
    pub base_url: String,
    /// Secret reference — NOT the actual API key.  Resolved via conduit-secret.
    pub upstream_key_ref: String,
    pub created_at: String,
    pub updated_at: String,
}

// ── Route ─────────────────────────────────────────────────────────────────────

/// Raw DB row for a route record.
///
/// `targets_json` and `retry_policy_json` are serialised JSON blobs that map to
/// `Vec<conduit_router::table::RouteTarget>` and
/// `conduit_router::policy::RetryPolicy` respectively.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RouteRow {
    pub id: String,
    /// The virtual alias callers use (e.g. `"gpt-4o"`, `"fast"`).
    pub match_alias: String,
    /// `"fixed"` or `"fallback"`.
    pub strategy: String,
    /// JSON-encoded `Vec<RouteTarget>`.
    pub targets_json: String,
    /// JSON-encoded `RetryPolicy`.
    pub retry_policy_json: String,
    pub enabled: bool,
    pub created_at: String,
    pub updated_at: String,
}

// ── Downstream key ────────────────────────────────────────────────────────────

/// Raw DB row for a downstream API key (issued to callers of the gateway).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DownstreamKeyRow {
    pub id: String,
    /// Human-readable label.
    pub name: String,
    /// BLAKE3 hash of the raw key bytes (hex-encoded).  Used for fast lookup.
    pub key_hash: String,
    /// JSON-encoded `Vec<String>` — model aliases this key may access.
    /// An empty array means "all models allowed".
    pub model_whitelist: String,
    pub monthly_budget_usd: Option<f64>,
    pub rate_limit_rpm: Option<i64>,
    pub enabled: bool,
    pub created_at: String,
    pub updated_at: String,
}

// ── Pricing ───────────────────────────────────────────────────────────────────

/// A single pricing row read from DB or pricing.json.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PricingRow {
    pub provider_kind: String,
    pub model_id: String,
    pub input_per_mtok: f64,
    pub output_per_mtok: f64,
    pub cache_read_per_mtok: Option<f64>,
    pub cache_write_per_mtok: Option<f64>,
    pub reasoning_per_mtok: Option<f64>,
    /// ISO 8601 date from which this pricing is valid.
    pub effective_from: String,
}

// ── Usage record (per-request consumption ledger) ─────────────────────────────

/// One completed gateway request's token + cost footprint.
///
/// Stored independently of the append-only trace log so spend remains available
/// when tracing is disabled.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UsageRecordRow {
    pub id: String,
    pub ts: String,
    /// Correlates with pipeline `trace_id` when traces are enabled; always set.
    pub request_id: String,
    pub downstream_key_id: Option<String>,
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
    pub cost_usd: f64,
    pub stream: bool,
}

// ── App event ─────────────────────────────────────────────────────────────────

/// Structured event entry (startup, config reload, downgrade warning, etc.).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppEventRow {
    pub id: String,
    pub ts: String,
    pub kind: String,
    pub message: String,
    pub metadata_json: Option<String>,
}
