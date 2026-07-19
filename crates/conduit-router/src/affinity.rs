//! In-process **session-scoped** provider affinity.
//!
//! Pins map `(session_id, alias)` → last successful `provider_id`.
//! This is the default base layer under multi-target and pool scheduling:
//! when a session pin is present and still eligible, it wins on attempt 0.
//!
//! Downstream API keys must **not** be used as the affinity key — one key may
//! serve many users/agents; pinning by key collapses a pool onto one account.

use std::collections::HashMap;

use parking_lot::Mutex;

/// Process-local session affinity pins. Not persisted across restarts.
#[derive(Debug, Default)]
pub struct AffinityStore {
    /// `(session_id, alias_lower)` → `provider_id`
    pins: Mutex<HashMap<(String, String), String>>,
}

impl AffinityStore {
    pub fn new() -> Self {
        Self::default()
    }

    fn pin_key(session_id: &str, alias: &str) -> (String, String) {
        (session_id.to_string(), alias.to_ascii_lowercase())
    }

    /// Preferred provider for this session + model alias, if any.
    pub fn preferred(&self, session_id: &str, alias: &str) -> Option<String> {
        let sid = session_id.trim();
        if sid.is_empty() {
            return None;
        }
        self.pins
            .lock()
            .get(&Self::pin_key(sid, alias))
            .cloned()
    }

    /// Remember a successful provider for future requests in this session.
    ///
    /// No-ops when `session_id` is empty (no false pins without a session).
    pub fn remember(&self, session_id: &str, alias: &str, provider_id: &str) {
        let sid = session_id.trim();
        if sid.is_empty() || alias.is_empty() || provider_id.is_empty() {
            return;
        }
        self.pins.lock().insert(
            Self::pin_key(sid, alias),
            provider_id.to_string(),
        );
    }

    /// Drop a pin (e.g. after failover when the preferred target is gone).
    pub fn forget(&self, session_id: &str, alias: &str) {
        let sid = session_id.trim();
        if sid.is_empty() {
            return;
        }
        self.pins.lock().remove(&Self::pin_key(sid, alias));
    }

    /// Number of active pins (tests / diagnostics).
    pub fn len(&self) -> usize {
        self.pins.lock().len()
    }

    pub fn is_empty(&self) -> bool {
        self.pins.lock().is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remember_and_preferred_are_case_insensitive_on_alias() {
        let s = AffinityStore::new();
        s.remember("sess-1", "GPT-5.6", "prov-a");
        assert_eq!(s.preferred("sess-1", "gpt-5.6").as_deref(), Some("prov-a"));
        assert_eq!(s.preferred("sess-2", "gpt-5.6"), None);
    }

    #[test]
    fn empty_session_never_pins() {
        let s = AffinityStore::new();
        s.remember("", "alias", "p1");
        s.remember("   ", "alias", "p1");
        assert!(s.is_empty());
        assert_eq!(s.preferred("", "alias"), None);
    }

    #[test]
    fn session_pins_are_independent_of_each_other() {
        let s = AffinityStore::new();
        s.remember("s-a", "claude", "acct-1");
        s.remember("s-b", "claude", "acct-2");
        assert_eq!(s.preferred("s-a", "claude").as_deref(), Some("acct-1"));
        assert_eq!(s.preferred("s-b", "claude").as_deref(), Some("acct-2"));
    }
}
