//! Last-seen upstream rate-limit / quota signals per provider.
//!
//! OAuth subscription products rarely expose a clean “remaining %” API.
//! We capture what we can:
//! - Response headers (`anthropic-ratelimit-*`, `retry-after`, …)
//! - Error bodies (`usage_limit_reached` + `resets_in_seconds`)
//!
//! Console can query snapshots; “refresh” re-reads the store (and optionally
//! clears stale cooldown). Probing upstream with a dummy request is left to
//! the operator (next real call updates headers).

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
    /// Session (≈5h) remaining percent 0–100 from OAuth usage API.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_remaining_pct: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_resets_at: Option<String>,
    /// Weekly (≈7d) remaining percent 0–100 from OAuth usage API.
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
        self.inner.lock().get(provider_id.trim()).cloned()
    }

    pub fn list(&self) -> Vec<QuotaSnapshot> {
        self.inner.lock().values().cloned().collect()
    }

    pub fn clear(&self, provider_id: &str) -> bool {
        self.inner.lock().remove(provider_id.trim()).is_some()
    }

    /// Ingest Anthropic-style rate-limit response headers.
    pub fn record_headers(
        &self,
        provider_id: &str,
        headers: impl IntoIterator<Item = (String, String)>,
    ) {
        let mut details = HashMap::new();
        let mut requests_remaining = None;
        let mut requests_limit = None;
        let mut tokens_remaining = None;
        let mut tokens_limit = None;
        let mut resets_in_seconds = None;
        let mut resets_at = None;

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
        {
            return;
        }

        self.put(QuotaSnapshot {
            provider_id: provider_id.to_string(),
            captured_at: chrono_now(),
            source: "response_headers".into(),
            requests_remaining,
            requests_limit,
            tokens_remaining,
            tokens_limit,
            resets_in_seconds,
            resets_at,
            session_remaining_pct: None,
            session_resets_at: None,
            weekly_remaining_pct: None,
            weekly_resets_at: None,
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

    /// Compact remaining label for list UIs: `5h 95% · 7d 66%`, Grok `mo 72%`, or header fallbacks.
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
}
