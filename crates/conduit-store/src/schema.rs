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
    ///
    /// Canonical form: `secret://upstream_key/{provider_id}`. The secret binding
    /// lives on the provider; routes do not choose a separate key id.
    pub upstream_key_ref: String,
    pub created_at: String,
    pub updated_at: String,
    /// Soft-delete timestamp (RFC 3339). `None` = active.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deleted_at: Option<String>,
}

/// Resolve the secret-backend id from a provider's `upstream_key_ref`.
///
/// Accepts `secret://upstream_key/{id}`, bare `upstream_key/{id}`, a raw id, or
/// empty (falls back to `provider_id`).
pub fn secret_key_id_from_ref(upstream_key_ref: &str, provider_id: &str) -> String {
    let r = upstream_key_ref.trim();
    if r.is_empty() {
        return provider_id.to_string();
    }
    if let Some(rest) = r.strip_prefix("secret://upstream_key/") {
        let id = rest.trim_matches('/');
        if !id.is_empty() {
            return id.to_string();
        }
    }
    if let Some(rest) = r.strip_prefix("upstream_key/") {
        let id = rest.trim_matches('/');
        if !id.is_empty() {
            return id.to_string();
        }
    }
    // Bare id or other forms: use as-is if it doesn't look like a URI.
    if !r.contains("://") {
        return r.to_string();
    }
    provider_id.to_string()
}

#[cfg(test)]
mod secret_key_id_tests {
    use super::secret_key_id_from_ref;

    #[test]
    fn parses_canonical_ref() {
        assert_eq!(
            secret_key_id_from_ref("secret://upstream_key/abc", "fallback"),
            "abc"
        );
    }

    #[test]
    fn empty_ref_uses_provider_id() {
        assert_eq!(secret_key_id_from_ref("", "p1"), "p1");
        assert_eq!(secret_key_id_from_ref("   ", "p1"), "p1");
    }

    #[test]
    fn bare_id_passthrough() {
        assert_eq!(secret_key_id_from_ref("my-key", "p1"), "my-key");
    }
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
    /// `"fixed"`, `"fallback"`, or `"weighted"` (sticky pin is cross-cutting on the last two).
    pub strategy: String,
    /// JSON-encoded `Vec<RouteTarget>`.
    pub targets_json: String,
    /// JSON-encoded `RetryPolicy`.
    pub retry_policy_json: String,
    pub enabled: bool,
    pub created_at: String,
    pub updated_at: String,
    /// Soft-delete timestamp (RFC 3339). `None` = active.
    /// Distinct from [`Self::enabled`] (operational disable).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deleted_at: Option<String>,
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
    /// Soft-delete timestamp (RFC 3339). `None` = active.
    /// Distinct from [`Self::enabled`] (operational disable).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deleted_at: Option<String>,
}

// ── Pricing ───────────────────────────────────────────────────────────────────

/// A single pricing row read from DB or pricing.json.
///
/// Price-only: context / max-output limits live in [`ModelLimitsRow`], not here.
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

// ── Model limits (context window / max output) ────────────────────────────────

/// Per-model token limits, separate from pricing.
///
/// Populated from LiteLLM `max_input_tokens` / `max_output_tokens` (never from
/// LiteLLM `max_tokens` as context — that field is often max output).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ModelLimitsRow {
    pub provider_kind: String,
    pub model_id: String,
    /// Context window (max input tokens). Source: LiteLLM `max_input_tokens`.
    pub max_input_tokens: u64,
    /// Optional max completion / output tokens. Source: `max_output_tokens`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_output_tokens: Option<u64>,
}

// ── Usage record (per-request ledger: tokens/cost + outcome/timing/routing) ───

/// One finished gateway request (success, zero-token, or terminal failure).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UsageRecordRow {
    pub id: String,
    pub ts: String,
    /// Pipeline request correlation id (always set).
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
    /// Outcome: `ok` | `error` | `cancelled` | `partial`.
    #[serde(default = "default_usage_status")]
    pub status: String,
    pub error_class: Option<String>,
    pub http_status: Option<u16>,
    pub finish_reason: Option<String>,
    pub duration_ms: Option<u64>,
    /// Time-to-first-byte (ms). Null when no first byte was observed.
    pub ttfb_ms: Option<u64>,
    pub route_strategy: Option<String>,
    #[serde(default)]
    pub attempt_no: u32,
    #[serde(default = "default_attempt_count")]
    pub attempt_count: u32,
    pub session_id: Option<String>,
    pub affinity_hit: Option<bool>,
    pub pool_id: Option<String>,
    pub selected_reason: Option<String>,
}

fn default_usage_status() -> String {
    "ok".into()
}

fn default_attempt_count() -> u32 {
    1
}

/// One upstream try within a gateway request (retry / fallback chain).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UsageAttemptRow {
    pub id: String,
    pub request_id: String,
    pub attempt_no: u32,
    pub provider_id: Option<String>,
    pub provider_kind: Option<String>,
    pub model_id: Option<String>,
    pub status: String,
    pub error_class: Option<String>,
    pub http_status: Option<u16>,
    pub duration_ms: Option<u64>,
    pub ttfb_ms: Option<u64>,
    /// `initial` | `retry` | etc.
    pub reason: Option<String>,
    pub ts: String,
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
