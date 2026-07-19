//! Last-seen upstream rate-limit / quota signals per provider.
//!
//! OAuth subscription products rarely expose a clean “remaining %” API.
//! We capture what we can:
//! - Response headers (`anthropic-ratelimit-*`, `retry-after`, …)
//! - Claude OAuth **unified** headers on real chat responses
//!   (`anthropic-ratelimit-unified-5h-utilization` / `…-7d-utilization`)
//! - Error bodies (`usage_limit_reached` + `resets_in_seconds`)
//! - Proactive OAuth usage/billing probes when Anthropic's `/api/oauth/usage`
//!   is not 429-limited
//!
//! Console can query snapshots; “refresh” probes OAuth APIs when available.
//! Header-derived remaining survives failed probes and is updated on each
//! successful upstream chat response.

use std::collections::HashMap;

use parking_lot::Mutex;
use serde::Serialize;

/// One captured quota / rate-limit observation.
#[derive(Debug, Clone, Serialize)]
pub struct QuotaSnapshot {
    pub provider_id: String,
    /// RFC3339 capture time.
    pub captured_at: String,
    pub source: String,
    /// Normalized limit fields when known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub requests_remaining: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub requests_limit: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tokens_remaining: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tokens_limit: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resets_in_seconds: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resets_at: Option<String>,
    /// Session (≈5h, Claude) remaining percent 0–100.
    /// From OAuth usage probe and/or `anthropic-ratelimit-unified-5h-utilization`.
    /// Codex no longer exposes a session window (weekly only).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_remaining_pct: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_resets_at: Option<String>,
    /// Weekly (≈7d) remaining percent 0–100.
    /// From OAuth usage probe and/or `anthropic-ratelimit-unified-7d-utilization`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub weekly_remaining_pct: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub weekly_resets_at: Option<String>,
    /// Raw header / body fragments for debugging.
    pub details: HashMap<String, String>,
}

#[derive(Debug, Default)]
pub struct UpstreamQuotaStore {
    inner: Mutex<HashMap<String, QuotaSnapshot>>,
}

impl UpstreamQuotaStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn put(&self, snap: QuotaSnapshot) {
        let id = snap.provider_id.clone();
        if id.is_empty() {
            return;
        }
        self.inner.lock().insert(id, snap);
    }

    pub fn get(&self, provider_id: &str) -> Option<QuotaSnapshot> {
        self.inner
            .lock()
            .get(provider_id.trim())
            .cloned()
            .map(enrich_unified_from_details)
    }

    pub fn list(&self) -> Vec<QuotaSnapshot> {
        self.inner
            .lock()
            .values()
            .cloned()
            .map(enrich_unified_from_details)
            .collect()
    }

    pub fn clear(&self, provider_id: &str) -> bool {
        self.inner.lock().remove(provider_id.trim()).is_some()
    }

    /// Ingest Anthropic-style rate-limit response headers.
    ///
    /// Maps Claude OAuth **unified** utilization headers into session/weekly
    /// remaining so the TUI REMAINING column updates on real chat traffic even
    /// when `GET /api/oauth/usage` is 429-limited. Prior OAuth session/weekly
    /// values are preserved when a header batch omits those fields (so classic
    /// RPM headers alone do not wipe subscription %).
    pub fn record_headers(
        &self,
        provider_id: &str,
        headers: impl IntoIterator<Item = (String, String)>,
    ) {
        let prev = self.get(provider_id);
        let mut details = prev
            .as_ref()
            .map(|p| p.details.clone())
            .unwrap_or_default();
        let mut requests_remaining = None;
        let mut requests_limit = None;
        let mut tokens_remaining = None;
        let mut tokens_limit = None;
        let mut resets_in_seconds = None;
        let mut resets_at = None;
        let mut session_remaining_pct: Option<f64> = None;
        let mut session_resets_at: Option<String> = None;
        let mut weekly_remaining_pct: Option<f64> = None;
        let mut weekly_resets_at: Option<String> = None;
        let mut saw_unified = false;

        for (k, v) in headers {
            let key = k.to_ascii_lowercase();
            let val = v.trim().to_string();
            if val.is_empty() {
                continue;
            }
            match key.as_str() {
                "anthropic-ratelimit-requests-remaining"
                | "x-ratelimit-remaining-requests"
                | "x-ratelimit-remaining" => {
                    requests_remaining = val.parse().ok();
                }
                "anthropic-ratelimit-requests-limit" | "x-ratelimit-limit-requests" => {
                    requests_limit = val.parse().ok();
                }
                "anthropic-ratelimit-tokens-remaining" | "x-ratelimit-remaining-tokens" => {
                    tokens_remaining = val.parse().ok();
                }
                "anthropic-ratelimit-tokens-limit" | "x-ratelimit-limit-tokens" => {
                    tokens_limit = val.parse().ok();
                }
                "anthropic-ratelimit-tokens-reset"
                | "anthropic-ratelimit-requests-reset"
                | "x-ratelimit-reset" => {
                    resets_at = Some(val.clone());
                }
                // Claude OAuth subscription windows (present on Messages API responses).
                "anthropic-ratelimit-unified-5h-utilization" => {
                    if let Some(used) = parse_utilization(&val) {
                        session_remaining_pct = Some(used_to_remaining(used));
                        saw_unified = true;
                    }
                }
                "anthropic-ratelimit-unified-5h-reset" => {
                    session_resets_at = Some(val.clone());
                    if resets_at.is_none() {
                        resets_at = Some(val.clone());
                    }
                    saw_unified = true;
                }
                "anthropic-ratelimit-unified-7d-utilization" => {
                    if let Some(used) = parse_utilization(&val) {
                        weekly_remaining_pct = Some(used_to_remaining(used));
                        saw_unified = true;
                    }
                }
                "anthropic-ratelimit-unified-7d-reset" => {
                    weekly_resets_at = Some(val.clone());
                    saw_unified = true;
                }
                "anthropic-ratelimit-unified-reset" => {
                    if resets_at.is_none() {
                        resets_at = Some(val.clone());
                    }
                }
                "retry-after" => {
                    if let Ok(secs) = val.parse::<u64>() {
                        resets_in_seconds = Some(secs);
                    } else {
                        resets_at = Some(val.clone());
                    }
                }
                _ => {}
            }
            if key.contains("ratelimit") || key == "retry-after" {
                details.insert(key, val);
            }
        }

        if details.is_empty()
            && requests_remaining.is_none()
            && tokens_remaining.is_none()
            && resets_in_seconds.is_none()
            && !saw_unified
        {
            return;
        }

        // Preserve prior subscription remaining when this batch has no unified fields
        // (e.g. classic RPM-only headers must not wipe a good OAuth snapshot).
        let session_remaining_pct = session_remaining_pct.or_else(|| {
            prev.as_ref().and_then(|p| p.session_remaining_pct)
        });
        let session_resets_at = session_resets_at.or_else(|| {
            prev.as_ref().and_then(|p| p.session_resets_at.clone())
        });
        let weekly_remaining_pct = weekly_remaining_pct.or_else(|| {
            prev.as_ref().and_then(|p| p.weekly_remaining_pct)
        });
        let weekly_resets_at = weekly_resets_at.or_else(|| {
            prev.as_ref().and_then(|p| p.weekly_resets_at.clone())
        });

        let source = if saw_unified {
            "response_headers_unified"
        } else {
            "response_headers"
        };

        self.put(QuotaSnapshot {
            provider_id: provider_id.to_string(),
            captured_at: chrono_now(),
            source: source.into(),
            requests_remaining,
            requests_limit,
            tokens_remaining,
            tokens_limit,
            resets_in_seconds,
            resets_at,
            session_remaining_pct,
            session_resets_at,
            weekly_remaining_pct,
            weekly_resets_at,
            details,
        });
    }

    /// Ingest 429 / usage_limit error body.
    pub fn record_error_body(&self, provider_id: &str, body: &str) {
        let mut details = HashMap::new();
        details.insert("body_excerpt".into(), body.chars().take(512).collect());
        let mut resets_in_seconds = None;
        let mut resets_at = None;
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(body) {
            let err = v.get("error").unwrap_or(&v);
            if let Some(secs) = err
                .get("resets_in_seconds")
                .and_then(|x| x.as_u64().or_else(|| x.as_i64().map(|i| i.max(0) as u64)))
            {
                resets_in_seconds = Some(secs);
            }
            if let Some(ts) = err.get("resets_at") {
                if let Some(n) = ts.as_i64().or_else(|| ts.as_u64().map(|u| u as i64)) {
                    resets_at = Some(n.to_string());
                } else if let Some(s) = ts.as_str() {
                    resets_at = Some(s.to_string());
                }
            }
            if let Some(t) = err.get("type").and_then(|x| x.as_str()) {
                details.insert("error_type".into(), t.to_string());
            }
        }
        // Preserve prior OAuth session/weekly remaining when only recording a 429.
        let prev = self.get(provider_id);
        self.put(QuotaSnapshot {
            provider_id: provider_id.to_string(),
            captured_at: chrono_now(),
            source: "error_body".into(),
            requests_remaining: Some(0),
            requests_limit: None,
            tokens_remaining: None,
            tokens_limit: None,
            resets_in_seconds,
            resets_at,
            session_remaining_pct: prev.as_ref().and_then(|p| p.session_remaining_pct),
            session_resets_at: prev.as_ref().and_then(|p| p.session_resets_at.clone()),
            weekly_remaining_pct: prev.as_ref().and_then(|p| p.weekly_remaining_pct),
            weekly_resets_at: prev.as_ref().and_then(|p| p.weekly_resets_at.clone()),
            details,
        });
    }

    /// Record OAuth subscription remaining (Claude / Codex usage API).
    pub fn record_oauth_usage(
        &self,
        provider_id: &str,
        source: &str,
        session_remaining_pct: Option<f64>,
        session_resets_at: Option<String>,
        weekly_remaining_pct: Option<f64>,
        weekly_resets_at: Option<String>,
        details: HashMap<String, String>,
    ) {
        // When remaining is 0 on either window, surface requests_remaining=0 for cooldown UX.
        let depleted = session_remaining_pct
            .map(|p| p <= 0.0)
            .unwrap_or(false)
            || weekly_remaining_pct.map(|p| p <= 0.0).unwrap_or(false);
        self.put(QuotaSnapshot {
            provider_id: provider_id.to_string(),
            captured_at: chrono_now(),
            source: source.to_string(),
            requests_remaining: if depleted { Some(0) } else { None },
            requests_limit: None,
            tokens_remaining: None,
            tokens_limit: None,
            resets_in_seconds: None,
            resets_at: session_resets_at
                .clone()
                .or_else(|| weekly_resets_at.clone()),
            session_remaining_pct,
            session_resets_at,
            weekly_remaining_pct,
            weekly_resets_at,
            details,
        });
    }

    /// Compact remaining label for list UIs: Claude `5h·7d`, Codex `7d`, Grok `mo`, or header fallbacks.
    pub fn remaining_label(snap: &QuotaSnapshot) -> Option<String> {
        let billing = snap.source.to_ascii_lowercase().contains("billing")
            || snap.source.to_ascii_lowercase().contains("grok");
        let mut parts = Vec::new();
        if let Some(p) = snap.session_remaining_pct {
            if billing {
                parts.push(format!("credits {p:.0}%"));
            } else {
                parts.push(format!("5h {p:.0}%"));
            }
        }
        if let Some(p) = snap.weekly_remaining_pct {
            if billing {
                parts.push(format!("mo {p:.0}%"));
            } else {
                parts.push(format!("7d {p:.0}%"));
            }
        }
        if !parts.is_empty() {
            return Some(parts.join(" · "));
        }
        if let (Some(rem), Some(lim)) = (snap.requests_remaining, snap.requests_limit) {
            if lim > 0 {
                let pct = (rem as f64 / lim as f64) * 100.0;
                return Some(format!("req {pct:.0}% ({rem}/{lim})"));
            }
            return Some(format!("req {rem} left"));
        }
        if let (Some(rem), Some(lim)) = (snap.tokens_remaining, snap.tokens_limit) {
            if lim > 0 {
                let pct = (rem as f64 / lim as f64) * 100.0;
                return Some(format!("tok {pct:.0}%"));
            }
        }
        if snap.requests_remaining == Some(0) {
            return Some("exhausted".into());
        }
        None
    }
}

fn chrono_now() -> String {
    // Avoid hard chrono dep in router if not present — use system time RFC3339-ish.
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    // Store as unix for simplicity when chrono not available in this crate... 
    // conduit-router has no chrono — use ISO via simple format is hard; unix string is fine.
    format!("{secs}")
}

/// Backfill session/weekly remaining from stored unified header details.
///
/// Chat traffic may have recorded raw headers into `details` before this mapping
/// existed; re-derive on read so REMAINING works without a new probe/chat.
fn enrich_unified_from_details(mut snap: QuotaSnapshot) -> QuotaSnapshot {
    if snap.session_remaining_pct.is_none() {
        if let Some(used) = snap
            .details
            .get("anthropic-ratelimit-unified-5h-utilization")
            .and_then(|s| parse_utilization(s))
        {
            snap.session_remaining_pct = Some(used_to_remaining(used));
        }
    }
    if snap.session_resets_at.is_none() {
        if let Some(r) = snap
            .details
            .get("anthropic-ratelimit-unified-5h-reset")
            .cloned()
        {
            snap.session_resets_at = Some(r);
        }
    }
    if snap.weekly_remaining_pct.is_none() {
        if let Some(used) = snap
            .details
            .get("anthropic-ratelimit-unified-7d-utilization")
            .and_then(|s| parse_utilization(s))
        {
            snap.weekly_remaining_pct = Some(used_to_remaining(used));
        }
    }
    if snap.weekly_resets_at.is_none() {
        if let Some(r) = snap
            .details
            .get("anthropic-ratelimit-unified-7d-reset")
            .cloned()
        {
            snap.weekly_resets_at = Some(r);
        }
    }
    snap
}

/// Parse utilization from header text (`0.18`, `18`, `18.0`).
fn parse_utilization(s: &str) -> Option<f64> {
    s.trim().parse::<f64>().ok()
}

/// Normalize used capacity to 0–100 percent (fraction 0..=1 → ×100).
fn normalize_used_pct(used: f64) -> f64 {
    if used > 1.0 {
        used.clamp(0.0, 100.0)
    } else if used < 0.0 {
        0.0
    } else {
        (used * 100.0).clamp(0.0, 100.0)
    }
}

fn used_to_remaining(used: f64) -> f64 {
    (100.0 - normalize_used_pct(used)).clamp(0.0, 100.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn records_anthropic_headers() {
        let s = UpstreamQuotaStore::new();
        s.record_headers(
            "p1",
            [
                ("anthropic-ratelimit-requests-remaining".into(), "12".into()),
                ("anthropic-ratelimit-requests-limit".into(), "50".into()),
            ],
        );
        let q = s.get("p1").unwrap();
        assert_eq!(q.requests_remaining, Some(12));
        assert_eq!(q.requests_limit, Some(50));
    }

    #[test]
    fn records_usage_limit_body() {
        let s = UpstreamQuotaStore::new();
        s.record_error_body(
            "p2",
            r#"{"error":{"type":"usage_limit_reached","resets_in_seconds":90}}"#,
        );
        let q = s.get("p2").unwrap();
        assert_eq!(q.requests_remaining, Some(0));
        assert_eq!(q.resets_in_seconds, Some(90));
    }

    #[test]
    fn maps_claude_unified_utilization_headers_to_remaining() {
        let s = UpstreamQuotaStore::new();
        s.record_headers(
            "claude-1",
            [
                (
                    "anthropic-ratelimit-unified-5h-utilization".into(),
                    "0.02".into(),
                ),
                (
                    "anthropic-ratelimit-unified-5h-reset".into(),
                    "1784507400".into(),
                ),
                (
                    "anthropic-ratelimit-unified-7d-utilization".into(),
                    "0.18".into(),
                ),
                (
                    "anthropic-ratelimit-unified-7d-reset".into(),
                    "1784955600".into(),
                ),
            ],
        );
        let q = s.get("claude-1").unwrap();
        assert_eq!(q.source, "response_headers_unified");
        assert!((q.session_remaining_pct.unwrap() - 98.0).abs() < 0.01);
        assert!((q.weekly_remaining_pct.unwrap() - 82.0).abs() < 0.01);
        assert_eq!(q.session_resets_at.as_deref(), Some("1784507400"));
        assert_eq!(q.weekly_resets_at.as_deref(), Some("1784955600"));
        let label = UpstreamQuotaStore::remaining_label(&q).unwrap();
        assert!(label.contains("5h 98%"), "{label}");
        assert!(label.contains("7d 82%"), "{label}");
    }

    #[test]
    fn unified_headers_accept_percent_scale() {
        let s = UpstreamQuotaStore::new();
        s.record_headers(
            "claude-2",
            [(
                "anthropic-ratelimit-unified-7d-utilization".into(),
                "34".into(),
            )],
        );
        let q = s.get("claude-2").unwrap();
        assert!((q.weekly_remaining_pct.unwrap() - 66.0).abs() < 0.01);
    }

    #[test]
    fn rpm_only_headers_do_not_wipe_prior_oauth_remaining() {
        let s = UpstreamQuotaStore::new();
        s.record_oauth_usage(
            "claude-3",
            "oauth_usage_api",
            Some(100.0),
            Some("reset-5h".into()),
            Some(82.0),
            Some("reset-7d".into()),
            HashMap::new(),
        );
        s.record_headers(
            "claude-3",
            [
                ("anthropic-ratelimit-requests-remaining".into(), "9".into()),
                ("anthropic-ratelimit-requests-limit".into(), "50".into()),
            ],
        );
        let q = s.get("claude-3").unwrap();
        assert_eq!(q.requests_remaining, Some(9));
        assert!((q.session_remaining_pct.unwrap() - 100.0).abs() < 0.01);
        assert!((q.weekly_remaining_pct.unwrap() - 82.0).abs() < 0.01);
        assert_eq!(q.session_resets_at.as_deref(), Some("reset-5h"));
    }

    #[test]
    fn get_backfills_remaining_from_legacy_details_only_snapshot() {
        let s = UpstreamQuotaStore::new();
        let mut details = HashMap::new();
        details.insert(
            "anthropic-ratelimit-unified-5h-utilization".into(),
            "0.0".into(),
        );
        details.insert(
            "anthropic-ratelimit-unified-7d-utilization".into(),
            "0.18".into(),
        );
        s.put(QuotaSnapshot {
            provider_id: "legacy".into(),
            captured_at: "1".into(),
            source: "response_headers".into(),
            requests_remaining: None,
            requests_limit: None,
            tokens_remaining: None,
            tokens_limit: None,
            resets_in_seconds: None,
            resets_at: None,
            session_remaining_pct: None,
            session_resets_at: None,
            weekly_remaining_pct: None,
            weekly_resets_at: None,
            details,
        });
        let q = s.get("legacy").unwrap();
        assert!((q.session_remaining_pct.unwrap() - 100.0).abs() < 0.01);
        assert!((q.weekly_remaining_pct.unwrap() - 82.0).abs() < 0.01);
    }
}
