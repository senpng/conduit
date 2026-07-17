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

pub fn is_valid_user_id(user_id: &str) -> bool {
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

pub fn cached_user_id(api_key: &str) -> String {
    get_or_insert(user_cache(), api_key, generate_fake_user_id)
}
