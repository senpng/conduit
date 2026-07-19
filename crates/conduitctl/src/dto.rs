//! Typed DTOs for console HTTP responses/requests used by `ConsoleClient`.
//!
//! Field names mirror `conduitd` console handlers (not the UI). Keep deserialize
//! loose (`default`) where the daemon may omit optional columns.

use serde::{Deserialize, Serialize};
use serde_json::Value;

// ── Health ──────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct HealthResponse {
    pub status: String,
    #[serde(default)]
    pub version: String,
}


// ── Providers ───────────────────────────────────────────────────────────────

/// Body for `POST /console/providers` — must match daemon `CreateProviderBody`.
///
/// **No client-supplied `id`**: the daemon allocates a ULID.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CreateProviderBody {
    pub name: String,
    pub kind: String,
    pub base_url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
}

impl CreateProviderBody {
    pub fn new(
        name: impl Into<String>,
        kind: impl Into<String>,
        base_url: impl Into<String>,
    ) -> Self {
        Self {
            name: name.into(),
            kind: kind.into(),
            base_url: base_url.into(),
            api_key: None,
        }
    }

    pub fn with_api_key(mut self, api_key: impl Into<String>) -> Self {
        self.api_key = Some(api_key.into());
        self
    }
}

// ── Keys ────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateKeyBody {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model_whitelist: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rate_limit_rpm: Option<i64>,
}

// ── Routes ──────────────────────────────────────────────────────────────────

/// One upstream target for route create (POST `targets` array element).
///
/// Pool targets (multi-account): set `pool_kind` and/or `pool_id`, leave
/// `provider_id` empty. Example:
/// `{"pool_kind":"claude-oauth","model_id":"claude-sonnet-4","provider_kind":"claude-oauth"}`
///
/// Secrets are bound on the **provider** (`provider_id`), not on the route.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RouteTargetSpec {
    /// Empty for pool targets (`pool_kind` / `pool_id`).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub provider_id: String,
    pub model_id: String,
    #[serde(default)]
    pub provider_kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    /// Static JSON fields merged into the target's encoded upstream request.
    #[serde(default, skip_serializing_if = "serde_json::Map::is_empty")]
    pub request_overrides: serde_json::Map<String, Value>,
    /// Named pool or auto kind-pool id (e.g. `"claude-oauth"`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pool_id: Option<String>,
    /// All providers of this kind form the pool.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pool_kind: Option<String>,
}

/// Body for `POST /console/routes` — matches daemon `CreateRouteBody`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CreateRouteBody {
    pub match_alias: String,
    pub strategy: String,
    /// JSON array of targets — **not** `targets_json`.
    pub targets: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retry_policy: Option<Value>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct KeyCreateResponse {
    pub id: String,
    /// Raw bearer token — shown ONCE by the daemon.
    pub key: String,
    pub name: String,
    #[serde(default)]
    pub model_whitelist: Vec<String>,
    pub rate_limit_rpm: Option<i64>,
    #[serde(default)]
    pub created_at: String,
}

// ── Update bodies ───────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct UpdateProviderBody {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SetSecretBody {
    pub api_key: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct UpdateKeyBody {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model_whitelist: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rate_limit_rpm: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
}

// ── List / view rows (console responses) ────────────────────────────────────

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct ProviderView {
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub base_url: String,
    #[serde(default)]
    pub upstream_key_ref: String,
    #[serde(default)]
    pub created_at: String,
    #[serde(default)]
    pub updated_at: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct RouteView {
    pub id: String,
    #[serde(default)]
    pub match_alias: String,
    #[serde(default)]
    pub strategy: String,
    /// JSON string blob from SQLite (`targets_json` column).
    #[serde(default)]
    pub targets_json: String,
    #[serde(default)]
    pub retry_policy_json: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub created_at: String,
    #[serde(default)]
    pub updated_at: String,
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct KeyView {
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub model_whitelist: Value,
    pub rate_limit_rpm: Option<i64>,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub created_at: String,
    #[serde(default)]
    pub updated_at: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct UsageRecordView {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub ts: String,
    #[serde(default)]
    pub request_id: String,
    pub downstream_key_id: Option<String>,
    pub alias: Option<String>,
    pub provider_id: Option<String>,
    pub provider_kind: Option<String>,
    pub model_id: Option<String>,
    #[serde(default)]
    pub prompt_tokens: u32,
    #[serde(default)]
    pub completion_tokens: u32,
    #[serde(default)]
    pub total_tokens: u32,
    #[serde(default)]
    pub reasoning_tokens: u32,
    /// Prompt tokens served from provider cache (read).
    #[serde(default)]
    pub cache_read_tokens: u32,
    /// Tokens written into provider cache (Anthropic cache creation, etc.).
    #[serde(default)]
    pub cache_write_tokens: u32,
    #[serde(default)]
    pub cost_usd: f64,
    #[serde(default)]
    pub stream: bool,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct UsageListResponse {
    #[serde(default)]
    pub entries: Vec<UsageRecordView>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct UsageSummaryEntry {
    #[serde(default)]
    pub downstream_key_id: String,
    #[serde(default)]
    pub request_count: u64,
    #[serde(default)]
    pub total_usd: f64,
    #[serde(default)]
    pub prompt_tokens: u64,
    #[serde(default)]
    pub completion_tokens: u64,
    #[serde(default)]
    pub total_tokens: u64,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct UsageDayEntry {
    #[serde(default)]
    pub day: String,
    #[serde(default)]
    pub request_count: u64,
    #[serde(default)]
    pub total_usd: f64,
    #[serde(default)]
    pub total_tokens: u64,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct UsageModelEntry {
    #[serde(default)]
    pub label: String,
    #[serde(default)]
    pub provider_kind: String,
    #[serde(default)]
    pub request_count: u64,
    #[serde(default)]
    pub total_usd: f64,
    #[serde(default)]
    pub total_tokens: u64,
}

/// Model breakdown for one downstream key (from `by_key_model`).
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct UsageKeyModelEntry {
    #[serde(default)]
    pub downstream_key_id: String,
    #[serde(default)]
    pub label: String,
    #[serde(default)]
    pub provider_kind: String,
    #[serde(default)]
    pub request_count: u64,
    #[serde(default)]
    pub total_usd: f64,
    #[serde(default)]
    pub total_tokens: u64,
}

/// Model breakdown for one UTC day (from `by_day_model`).
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct UsageDayModelEntry {
    #[serde(default)]
    pub day: String,
    #[serde(default)]
    pub label: String,
    #[serde(default)]
    pub provider_kind: String,
    #[serde(default)]
    pub request_count: u64,
    #[serde(default)]
    pub total_usd: f64,
    #[serde(default)]
    pub total_tokens: u64,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct UsageSummaryView {
    #[serde(default)]
    pub period: String,
    #[serde(default)]
    pub total_usd: f64,
    #[serde(default)]
    pub request_count: u64,
    pub key_id: Option<String>,
    #[serde(default)]
    pub entries: Vec<UsageSummaryEntry>,
    #[serde(default)]
    pub by_day: Vec<UsageDayEntry>,
    #[serde(default)]
    pub by_model: Vec<UsageModelEntry>,
    /// Per-key model rollups for Usage → by key detail.
    #[serde(default)]
    pub by_key_model: Vec<UsageKeyModelEntry>,
    /// Per-day model rollups for Usage → by day detail.
    #[serde(default)]
    pub by_day_model: Vec<UsageDayModelEntry>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct PricingView {
    #[serde(default)]
    pub provider_kind: String,
    #[serde(default)]
    pub model_id: String,
    #[serde(default)]
    pub input_per_mtok: f64,
    #[serde(default)]
    pub output_per_mtok: f64,
    pub cache_read_per_mtok: Option<f64>,
    pub cache_write_per_mtok: Option<f64>,
    pub reasoning_per_mtok: Option<f64>,
    #[serde(default)]
    pub effective_from: String,
}

/// Body for `PUT /console/pricing/overrides` — operator layer (`pricing.json`).
///
/// Prices are USD per million tokens (same unit as tokscale custom-pricing and
/// Conduit's merged pricing table).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct UpsertPricingOverrideBody {
    pub provider_kind: String,
    pub model_id: String,
    pub input_per_mtok: f64,
    pub output_per_mtok: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_read_per_mtok: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_write_per_mtok: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning_per_mtok: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub effective_from: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct OAuthSessionView {
    #[serde(default)]
    pub session_id: String,
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub status: String,
    pub auth_url: Option<String>,
    pub user_code: Option<String>,
    pub verification_uri: Option<String>,
    pub verification_uri_complete: Option<String>,
    pub expires_in: Option<u64>,
    pub provider_id: Option<String>,
    pub email: Option<String>,
    pub error: Option<String>,
}

// ── Provider secret reveal (console decrypt) ────────────────────────────────

/// `GET /console/providers/{id}/secret` — decrypted upstream credential.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Default)]
pub struct ProviderSecretView {
    #[serde(default)]
    pub provider_id: String,
    #[serde(default)]
    pub provider_name: String,
    #[serde(default)]
    pub provider_kind: String,
    /// `"api_key"` or `"oauth"`.
    #[serde(default)]
    pub secret_kind: String,
    #[serde(default)]
    pub key_id: String,
    /// Present when `secret_kind == "api_key"`.
    pub api_key: Option<String>,
    /// Present when `secret_kind == "oauth"`.
    pub oauth: Option<ProviderOauthSecretView>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Default)]
pub struct ProviderOauthSecretView {
    #[serde(default, rename = "type")]
    pub provider_type: String,
    #[serde(default)]
    pub auth_kind: String,
    #[serde(default)]
    pub access_token: String,
    #[serde(default)]
    pub refresh_token: String,
    pub id_token: Option<String>,
    pub token_type: Option<String>,
    /// Access-token expiry (RFC3339).
    pub expired: Option<String>,
    pub last_refresh: Option<String>,
    pub email: Option<String>,
    pub account_id: Option<String>,
    pub plan_type: Option<String>,
    pub organization_id: Option<String>,
    pub organization_name: Option<String>,
    pub sub: Option<String>,
    pub base_url: Option<String>,
    pub token_endpoint: Option<String>,
    pub proxy_url: Option<String>,
    pub using_api: Option<bool>,
    #[serde(default)]
    pub extra: serde_json::Map<String, Value>,
}

// ── Upstream quota / cooldown (OAuth remaining) ─────────────────────────────

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Default)]
pub struct QuotaSnapshotView {
    #[serde(default)]
    pub provider_id: String,
    #[serde(default)]
    pub captured_at: String,
    #[serde(default)]
    pub source: String,
    pub requests_remaining: Option<u64>,
    pub requests_limit: Option<u64>,
    pub tokens_remaining: Option<u64>,
    pub tokens_limit: Option<u64>,
    pub resets_in_seconds: Option<u64>,
    pub resets_at: Option<String>,
    /// Session (≈5h) remaining percent 0–100.
    pub session_remaining_pct: Option<f64>,
    pub session_resets_at: Option<String>,
    /// Weekly (≈7d) remaining percent 0–100.
    pub weekly_remaining_pct: Option<f64>,
    pub weekly_resets_at: Option<String>,
    #[serde(default)]
    pub details: std::collections::HashMap<String, String>,
}

impl QuotaSnapshotView {
    /// Compact remaining label: `5h 95% · 7d 66%`, Grok `mo 72%`, or header fallbacks.
    pub fn remaining_label(&self) -> Option<String> {
        let billing = self.source.to_ascii_lowercase().contains("billing")
            || self.source.to_ascii_lowercase().contains("grok");
        let mut parts = Vec::new();
        if let Some(p) = self.session_remaining_pct {
            if billing {
                parts.push(format!("credits {p:.0}%"));
            } else {
                parts.push(format!("5h {p:.0}%"));
            }
        }
        if let Some(p) = self.weekly_remaining_pct {
            if billing {
                parts.push(format!("mo {p:.0}%"));
            } else {
                parts.push(format!("7d {p:.0}%"));
            }
        }
        if !parts.is_empty() {
            return Some(parts.join(" · "));
        }
        if let (Some(rem), Some(lim)) = (self.requests_remaining, self.requests_limit) {
            if lim > 0 {
                let pct = (rem as f64 / lim as f64) * 100.0;
                return Some(format!("req {pct:.0}%"));
            }
            return Some(format!("req {rem} left"));
        }
        if self.requests_remaining == Some(0) {
            return Some("exhausted".into());
        }
        None
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq, Default)]
pub struct CooldownView {
    #[serde(default)]
    pub provider_id: String,
    #[serde(default)]
    pub remaining_secs: u64,
    #[serde(default)]
    pub reason: String,
    #[serde(default)]
    pub status_code: u16,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Default)]
pub struct QuotaListResponse {
    #[serde(default)]
    pub entries: Vec<QuotaSnapshotView>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Default)]
pub struct CooldownListResponse {
    #[serde(default)]
    pub entries: Vec<CooldownView>,
}

// ── Generic JSON helpers ────────────────────────────────────────────────────

/// Loose envelope when we only need pretty-print / passthrough.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct JsonValue(pub Value);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_provider_body_serializes_without_id() {
        let body = CreateProviderBody::new("prod-openai", "openai", "https://api.openai.com/v1")
            .with_api_key("sk-test");
        let v = serde_json::to_value(&body).unwrap();
        assert!(v.get("id").is_none(), "must not send client id: {v}");
        assert_eq!(v["name"], "prod-openai");
        assert_eq!(v["kind"], "openai");
        assert_eq!(v["base_url"], "https://api.openai.com/v1");
        assert_eq!(v["api_key"], "sk-test");
    }

    #[test]
    fn create_provider_body_omits_absent_api_key() {
        let body = CreateProviderBody::new("p", "anthropic", "https://api.anthropic.com");
        let s = serde_json::to_string(&body).unwrap();
        assert!(
            !s.contains("api_key"),
            "optional api_key must be skipped: {s}"
        );
        assert!(!s.contains("\"id\""), "must not include id: {s}");
    }

    #[test]
    fn health_response_deserializes() {
        let raw = r#"{"status":"ok","version":"0.1.0"}"#;
        let h: HealthResponse = serde_json::from_str(raw).unwrap();
        assert_eq!(h.status, "ok");
        assert_eq!(h.version, "0.1.0");
    }

    #[test]
    fn provider_view_deserializes() {
        let raw = r#"{
            "id":"01ABC","name":"oai","kind":"openai",
            "base_url":"https://api.openai.com/v1",
            "upstream_key_ref":"secret://upstream_key/01ABC",
            "created_at":"t","updated_at":"t"
        }"#;
        let p: ProviderView = serde_json::from_str(raw).unwrap();
        assert_eq!(p.id, "01ABC");
        assert_eq!(p.kind, "openai");
    }

    #[test]
    fn usage_list_response_wraps_entries() {
        let raw = r#"{"entries":[{"id":"1","ts":"t","request_id":"r","cost_usd":0.01}]}"#;
        let list: UsageListResponse = serde_json::from_str(raw).unwrap();
        assert_eq!(list.entries.len(), 1);
        assert!((list.entries[0].cost_usd - 0.01).abs() < f64::EPSILON);
        assert_eq!(list.entries[0].cache_read_tokens, 0);
        assert_eq!(list.entries[0].cache_write_tokens, 0);
    }

    #[test]
    fn usage_record_deserializes_cache_tokens() {
        let raw = r#"{
            "id":"1","ts":"2026-07-01T00:00:00Z","request_id":"r",
            "prompt_tokens":100,"completion_tokens":20,"total_tokens":120,
            "reasoning_tokens":5,
            "cache_read_tokens":80,"cache_write_tokens":40,
            "cost_usd":0.01,"stream":true
        }"#;
        let u: UsageRecordView = serde_json::from_str(raw).unwrap();
        assert_eq!(u.cache_read_tokens, 80);
        assert_eq!(u.cache_write_tokens, 40);
        assert_eq!(u.reasoning_tokens, 5);
        assert_eq!(u.prompt_tokens, 100);
    }

    #[test]
    fn route_target_spec_skips_empty_overrides() {
        let t = RouteTargetSpec {
            provider_id: "p".into(),
            model_id: "m".into(),
            provider_kind: "openai".into(),
            base_url: None,
            request_overrides: Default::default(),
            pool_id: None,
            pool_kind: None,
        };
        let s = serde_json::to_string(&t).unwrap();
        assert!(!s.contains("request_overrides"), "{s}");
    }
}
