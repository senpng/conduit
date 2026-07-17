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
    /// Whether new traces are being recorded (when present).
    #[serde(default)]
    pub trace_enabled: Option<bool>,
}

// ── Settings ────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct SettingsResponse {
    pub trace: TraceSettingsDto,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct TraceSettingsDto {
    pub enabled: bool,
    #[serde(default)]
    pub config_default: Option<bool>,
    #[serde(default)]
    pub runtime_override: Option<bool>,
    #[serde(default)]
    pub max_segment_mb: Option<u64>,
    #[serde(default)]
    pub max_db_size_mb: Option<u64>,
    #[serde(default)]
    pub retention_days: Option<u32>,
}

#[derive(Debug, Clone, Serialize)]
pub struct UpdateSettingsBody {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trace: Option<UpdateTraceSettingsBody>,
}

#[derive(Debug, Clone, Serialize)]
pub struct UpdateTraceSettingsBody {
    pub enabled: bool,
}

// ── Traces ──────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct TraceListResponse {
    #[serde(default)]
    pub traces: Vec<TraceIndexRowDto>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct TraceIndexRowDto {
    pub id: String,
    #[serde(default)]
    pub trace_id: String,
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub alias: String,
    #[serde(default)]
    pub status_code: i64,
    #[serde(default)]
    pub latency_ms: i64,
    #[serde(default)]
    pub cost_usd: f64,
    pub provider_id: Option<String>,
    pub model_id: Option<String>,
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
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RouteTargetSpec {
    pub provider_id: String,
    pub model_id: String,
    pub upstream_key_id: String,
    pub provider_kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    /// Static JSON fields merged into the target's encoded upstream request.
    #[serde(default, skip_serializing_if = "serde_json::Map::is_empty")]
    pub request_overrides: serde_json::Map<String, Value>,
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
    fn trace_list_response_unwraps_traces_wrapper() {
        let raw = r#"{"traces":[{"id":"01A","alias":"gpt","status_code":200,"latency_ms":12,"cost_usd":0.01}]}"#;
        let parsed: TraceListResponse = serde_json::from_str(raw).unwrap();
        assert_eq!(parsed.traces.len(), 1);
        assert_eq!(parsed.traces[0].id, "01A");
        assert_eq!(parsed.traces[0].alias, "gpt");
    }

    #[test]
    fn health_response_deserializes() {
        let raw = r#"{"status":"ok","version":"0.1.0"}"#;
        let h: HealthResponse = serde_json::from_str(raw).unwrap();
        assert_eq!(h.status, "ok");
        assert_eq!(h.version, "0.1.0");
        assert_eq!(h.trace_enabled, None);
    }

    #[test]
    fn health_response_with_trace_enabled() {
        let raw = r#"{"status":"ok","version":"0.1.0","trace_enabled":false}"#;
        let h: HealthResponse = serde_json::from_str(raw).unwrap();
        assert_eq!(h.trace_enabled, Some(false));
    }
}
