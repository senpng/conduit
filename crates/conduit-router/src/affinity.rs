//! In-process sticky provider affinity.
//!
//! Keyed by `(downstream_key_id, alias)` → last successful `provider_id`.
//! Cross-cutting preference for multi-target
//! [`Fallback`](crate::table::RoutingStrategy::Fallback) and
//! [`Weighted`](crate::table::RoutingStrategy::Weighted) routes.

use std::collections::HashMap;

use parking_lot::Mutex;

/// Process-local affinity pins. Not persisted across restarts.
#[derive(Debug, Default)]
pub struct AffinityStore {
    /// `(key_id, alias_lower)` → `provider_id`
    pins: Mutex<HashMap<(String, String), String>>,
}

impl AffinityStore {
    pub fn new() -> Self {
        Self::default()
    }

    fn pin_key(key_id: &str, alias: &str) -> (String, String) {
        (key_id.to_string(), alias.to_ascii_lowercase())
    }

    /// Preferred provider for this downstream key + model alias, if any.
    pub fn preferred(&self, key_id: &str, alias: &str) -> Option<String> {
        self.pins.lock().get(&Self::pin_key(key_id, alias)).cloned()
    }

    /// Remember a successful provider for future requests.
    pub fn remember(&self, key_id: &str, alias: &str, provider_id: &str) {
        if key_id.is_empty() || alias.is_empty() || provider_id.is_empty() {
            return;
        }
        self.pins
            .lock()
            .insert(Self::pin_key(key_id, alias), provider_id.to_string());
    }

    /// Drop a pin (e.g. after the preferred target is removed from the route).
    pub fn forget(&self, key_id: &str, alias: &str) {
        self.pins.lock().remove(&Self::pin_key(key_id, alias));
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
        s.remember("k1", "GPT-5.6", "prov-a");
        assert_eq!(s.preferred("k1", "gpt-5.6").as_deref(), Some("prov-a"));
        assert_eq!(s.preferred("k2", "gpt-5.6"), None);
    }
}
