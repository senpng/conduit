//! In-memory upstream provider cooldown (CLIProxyAPI quota / 429 cooldown parity).
//!
//! When an upstream returns 429 / usage_limit, the provider is marked cooling so
//! multi-target routes skip it until `until` elapses.

use std::{
    collections::HashMap,
    time::{Duration, Instant},
};

use parking_lot::Mutex;
use serde::Serialize;

/// Default cooldown when 429 body has no reset timing.
pub const DEFAULT_COOLDOWN: Duration = Duration::from_secs(60);
/// Cap cooldown so a bad parse cannot ban forever.
pub const MAX_COOLDOWN: Duration = Duration::from_secs(24 * 60 * 60);
/// Floor so we do not thrash on 1s resets.
pub const MIN_COOLDOWN: Duration = Duration::from_secs(5);

/// Public view of a cooldown entry (console / CLI).
#[derive(Debug, Clone, Serialize)]
pub struct CooldownView {
    pub provider_id: String,
    /// Seconds remaining (0 if expired).
    pub remaining_secs: u64,
    pub reason: String,
    pub status_code: u16,
}

#[derive(Debug, Clone)]
struct Entry {
    until: Instant,
    reason: String,
    status_code: u16,
}

/// Process-wide (or daemon-scoped) cooldown registry keyed by `provider_id`.
#[derive(Debug, Default)]
pub struct ProviderCooldownStore {
    inner: Mutex<HashMap<String, Entry>>,
}

impl ProviderCooldownStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// Mark provider cooling for `duration` (clamped).
    pub fn mark(&self, provider_id: &str, duration: Duration, reason: impl Into<String>, status_code: u16) {
        let id = provider_id.trim();
        if id.is_empty() {
            return;
        }
        let d = duration.clamp(MIN_COOLDOWN, MAX_COOLDOWN);
        let mut map = self.inner.lock();
        map.insert(
            id.to_string(),
            Entry {
                until: Instant::now() + d,
                reason: reason.into(),
                status_code,
            },
        );
    }

    /// True if provider is currently in cooldown.
    pub fn is_cooling(&self, provider_id: &str) -> bool {
        self.remaining(provider_id).is_some()
    }

    /// Remaining cooldown duration if active.
    pub fn remaining(&self, provider_id: &str) -> Option<Duration> {
        let mut map = self.inner.lock();
        let id = provider_id.trim();
        let e = map.get(id)?;
        let now = Instant::now();
        if e.until <= now {
            map.remove(id);
            return None;
        }
        Some(e.until.saturating_duration_since(now))
    }

    /// Set of provider ids currently cooling (for route filtering).
    pub fn cooling_ids(&self) -> std::collections::HashSet<String> {
        let mut map = self.inner.lock();
        let now = Instant::now();
        map.retain(|_, e| e.until > now);
        map.keys().cloned().collect()
    }

    pub fn clear(&self, provider_id: &str) -> bool {
        self.inner.lock().remove(provider_id.trim()).is_some()
    }

    pub fn clear_all(&self) {
        self.inner.lock().clear();
    }

    pub fn list(&self) -> Vec<CooldownView> {
        let mut map = self.inner.lock();
        let now = Instant::now();
        map.retain(|_, e| e.until > now);
        map.iter()
            .map(|(id, e)| CooldownView {
                provider_id: id.clone(),
                remaining_secs: e.until.saturating_duration_since(now).as_secs(),
                reason: e.reason.clone(),
                status_code: e.status_code,
            })
            .collect()
    }
}

/// Parse cooldown duration from upstream error body (JSON or plain text).
///
/// Prefers Codex `usage_limit_reached` + `resets_in_seconds` / `resets_at`.
/// Falls back to Anthropic-style messages or [`DEFAULT_COOLDOWN`].
pub fn parse_cooldown_duration(body: &str) -> Duration {
    let body = body.trim();
    if body.is_empty() {
        return DEFAULT_COOLDOWN;
    }

    // Try JSON first.
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(body) {
        if let Some(d) = duration_from_json(&v) {
            return d.clamp(MIN_COOLDOWN, MAX_COOLDOWN);
        }
        // Nested error object
        if let Some(err) = v.get("error") {
            if let Some(d) = duration_from_json(err) {
                return d.clamp(MIN_COOLDOWN, MAX_COOLDOWN);
            }
        }
    }

    // Plain-text "resets_in_seconds": N
    if let Some(secs) = extract_u64_near(body, "resets_in_seconds") {
        return Duration::from_secs(secs).clamp(MIN_COOLDOWN, MAX_COOLDOWN);
    }

    DEFAULT_COOLDOWN
}

/// True when body indicates subscription quota exhaustion (not transient RPM).
pub fn is_usage_limit_body(body: &str) -> bool {
    let lower = body.to_ascii_lowercase();
    lower.contains("usage_limit_reached")
        || lower.contains("usage limit")
        || lower.contains("quota exceeded")
        || lower.contains("hit your usage limit")
}

fn duration_from_json(v: &serde_json::Value) -> Option<Duration> {
    // resets_at unix seconds
    if let Some(ts) = v
        .get("resets_at")
        .and_then(|x| x.as_i64().or_else(|| x.as_u64().map(|u| u as i64)))
    {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .ok()?
            .as_secs() as i64;
        if ts > now {
            return Some(Duration::from_secs((ts - now) as u64));
        }
    }
    if let Some(secs) = v
        .get("resets_in_seconds")
        .and_then(|x| x.as_u64().or_else(|| x.as_i64().map(|i| i.max(0) as u64)))
    {
        if secs > 0 {
            return Some(Duration::from_secs(secs));
        }
    }
    // retry-after style
    if let Some(secs) = v
        .get("retry_after")
        .and_then(|x| x.as_u64().or_else(|| x.as_i64().map(|i| i.max(0) as u64)))
    {
        if secs > 0 {
            return Some(Duration::from_secs(secs));
        }
    }
    None
}

fn extract_u64_near(hay: &str, key: &str) -> Option<u64> {
    let lower = hay.to_ascii_lowercase();
    let key = key.to_ascii_lowercase();
    let idx = lower.find(&key)?;
    let tail = &hay[idx + key.len()..];
    let digits: String = tail
        .chars()
        .skip_while(|c| !c.is_ascii_digit())
        .take_while(|c| c.is_ascii_digit())
        .collect();
    if digits.is_empty() {
        None
    } else {
        digits.parse().ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mark_and_clear() {
        let s = ProviderCooldownStore::new();
        s.mark("p1", Duration::from_secs(30), "rate_limited", 429);
        assert!(s.is_cooling("p1"));
        assert!(s.clear("p1"));
        assert!(!s.is_cooling("p1"));
    }

    #[test]
    fn parse_usage_limit_json() {
        let body = r#"{"error":{"type":"usage_limit_reached","resets_in_seconds":120}}"#;
        assert!(is_usage_limit_body(body));
        assert_eq!(parse_cooldown_duration(body), Duration::from_secs(120));
    }

    #[test]
    fn parse_default_on_empty() {
        assert_eq!(parse_cooldown_duration(""), DEFAULT_COOLDOWN);
    }
}
