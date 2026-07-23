//! Stable per-token session / user id caches (CLIProxyAPI parity).

use std::{
    collections::HashMap,
    sync::{Mutex, OnceLock},
    time::{Duration, Instant},
};

use rand::RngCore;
use sha2::{Digest, Sha256};
use uuid::Uuid;

const SESSION_TTL: Duration = Duration::from_secs(3600);

struct CacheEntry {
    value: String,
    expire: Instant,
}

fn session_cache() -> &'static Mutex<HashMap<String, CacheEntry>> {
    static CACHE: OnceLock<Mutex<HashMap<String, CacheEntry>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn user_cache() -> &'static Mutex<HashMap<String, CacheEntry>> {
    static CACHE: OnceLock<Mutex<HashMap<String, CacheEntry>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn cache_key(api_key: &str) -> String {
    let sum = Sha256::digest(api_key.as_bytes());
    hex::encode(sum)
}

fn get_or_insert(
    cache: &Mutex<HashMap<String, CacheEntry>>,
    api_key: &str,
    generate: impl FnOnce() -> String,
) -> String {
    if api_key.is_empty() {
        return generate();
    }
    let key = cache_key(api_key);
    let now = Instant::now();
    let mut guard = cache.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(entry) = guard.get_mut(&key) {
        if entry.expire > now && !entry.value.is_empty() {
            entry.expire = now + SESSION_TTL;
            return entry.value.clone();
        }
    }
    let value = generate();
    guard.insert(
        key,
        CacheEntry {
            value: value.clone(),
            expire: now + SESSION_TTL,
        },
    );
    value
}

/// Stable session UUID per access token (X-Claude-Code-Session-Id).
pub fn cached_session_id(api_key: &str) -> String {
    get_or_insert(session_cache(), api_key, || Uuid::new_v4().to_string())
}

/// Claude Code format: `user_[64-hex]_account_[uuid]_session_[uuid]`.
///
/// Cloak **injection** still uses this classic string form (CLIProxyAPI
/// `generateFakeUserID` parity). Clients may also send a newer JSON-string
/// shape which we preserve — see [`is_valid_user_id`].
pub fn generate_fake_user_id() -> String {
    let mut hex_bytes = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut hex_bytes);
    format!(
        "user_{}_account_{}_session_{}",
        hex::encode(hex_bytes),
        Uuid::new_v4(),
        Uuid::new_v4()
    )
}

/// Whether `user_id` is a client-supplied Claude Code identity we must keep.
///
/// Accepts both CLIProxyAPI-recognized forms:
/// 1. Classic: `user_[64-hex]_account_[uuid]_session_[uuid]`
/// 2. JSON-string (new Claude Code):
///    `{"device_id":"...","account_uuid":"...","session_id":"..."}`
///
/// For the JSON-string form, a non-empty `session_id` is required (same as
/// CLIProxyAPI session extraction).
pub fn is_valid_user_id(user_id: &str) -> bool {
    let user_id = user_id.trim();
    if user_id.is_empty() {
        return false;
    }
    is_classic_claude_user_id(user_id) || is_json_string_user_id(user_id)
}

/// Classic Claude Code string form.
fn is_classic_claude_user_id(user_id: &str) -> bool {
    // user_[64 hex]_account_[uuid]_session_[uuid]
    if !user_id.starts_with("user_") {
        return false;
    }
    let rest = &user_id[5..];
    let Some((hex_part, after_hex)) = rest.split_once("_account_") else {
        return false;
    };
    if hex_part.len() != 64 || !hex_part.chars().all(|c| c.is_ascii_hexdigit()) {
        return false;
    }
    let Some((account, session)) = after_hex.split_once("_session_") else {
        return false;
    };
    Uuid::parse_str(account).is_ok() && Uuid::parse_str(session).is_ok()
}

/// JSON-string form: wire type is still a JSON string, content is an object.
///
/// Example:
/// `{"device_id":"…","account_uuid":"","session_id":"e26d4046-…"}`
fn is_json_string_user_id(user_id: &str) -> bool {
    session_id_from_json_user_id(user_id).is_some()
}

/// Extract `session_id` from a JSON-string (or raw JSON object text) user_id.
///
/// Returns `None` when the value is not a JSON object or `session_id` is empty.
pub fn session_id_from_json_user_id(user_id: &str) -> Option<String> {
    let user_id = user_id.trim();
    if !user_id.starts_with('{') {
        return None;
    }
    let v: serde_json::Value = serde_json::from_str(user_id).ok()?;
    v.get("session_id")
        .and_then(|s| s.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
}

/// Whether a wire `metadata.user_id` value should be preserved as-is.
///
/// Handles:
/// - classic / JSON-string (`Value::String`)
/// - rare object form `{"device_id", "account_uuid", "session_id"}` (`Value::Object`)
pub fn should_preserve_user_id(value: &serde_json::Value) -> bool {
    match value {
        serde_json::Value::String(s) => is_valid_user_id(s),
        serde_json::Value::Object(obj) => obj
            .get("session_id")
            .and_then(|s| s.as_str())
            .map(str::trim)
            .is_some_and(|s| !s.is_empty()),
        _ => false,
    }
}

pub fn cached_user_id(api_key: &str) -> String {
    get_or_insert(user_cache(), api_key, generate_fake_user_id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn classic_user_id_is_valid() {
        let uid = "user_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa_account_11111111-1111-1111-1111-111111111111_session_22222222-2222-2222-2222-222222222222";
        assert!(is_valid_user_id(uid));
        assert_eq!(
            session_id_from_json_user_id(uid),
            None,
            "classic form is not JSON"
        );
    }

    #[test]
    fn json_string_user_id_is_valid() {
        let uid = r#"{"device_id":"be82c3aee1e0c2d74535bacc85f9f559228f02dd8a17298cf522b71e6c375714","account_uuid":"","session_id":"e26d4046-0f88-4b09-bb5b-f863ab5fb24e"}"#;
        assert!(is_valid_user_id(uid));
        assert_eq!(
            session_id_from_json_user_id(uid).as_deref(),
            Some("e26d4046-0f88-4b09-bb5b-f863ab5fb24e")
        );
    }

    #[test]
    fn json_string_without_session_id_is_invalid() {
        let uid = r#"{"device_id":"abc123"}"#;
        assert!(!is_valid_user_id(uid));
        assert_eq!(session_id_from_json_user_id(uid), None);
    }

    #[test]
    fn bare_string_is_not_valid_claude_user_id() {
        assert!(!is_valid_user_id("plain-user"));
        assert!(!is_valid_user_id("user_short"));
    }

    #[test]
    fn preserve_object_form_with_session_id() {
        let v = json!({
            "device_id": "d1",
            "account_uuid": "",
            "session_id": "obj-sess"
        });
        assert!(should_preserve_user_id(&v));
        assert!(!should_preserve_user_id(&json!({"device_id": "d1"})));
        assert!(!should_preserve_user_id(&json!(42)));
    }

    #[test]
    fn generated_fake_matches_classic_shape() {
        let uid = generate_fake_user_id();
        assert!(is_classic_claude_user_id(&uid), "uid={uid}");
        assert!(is_valid_user_id(&uid));
    }
}
