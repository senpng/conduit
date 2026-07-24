//! Shared console error helpers and key minting.

use axum::{http::StatusCode, Json};
use serde_json::{json, Value};

// ── Error helper ─────────────────────────────────────────────────────────────

pub(crate) fn err(status: StatusCode, msg: impl Into<String>) -> (StatusCode, Json<Value>) {
    (status, Json(json!({"error": msg.into()})))
}

pub(crate) fn internal(e: impl std::fmt::Display) -> (StatusCode, Json<Value>) {
    err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
}

/// Reject nonsensical rate limits. The value is stored as `i64` but later cast
/// to `u32` for enforcement, so a negative wraps to a near-infinite limit
/// (rate-limiting silently disabled) and `0` rejects every request (the key is
/// bricked). Only a positive requests/minute is meaningful.
pub(crate) fn validate_rpm(rpm: Option<i64>) -> Result<(), &'static str> {
    match rpm {
        Some(v) if v < 1 => Err("rate_limit_rpm must be a positive number"),
        _ => Ok(()),
    }
}

/// Mint a new downstream bearer token: `sk_` + 64 hex chars (32 CSPRNG bytes).
///
/// Entropy comes from the OS via [`rand::rngs::OsRng`]. BLAKE3 is used only later
/// to store `key_hash` in SQLite — never as a substitute for the random source.
pub(crate) fn generate_downstream_raw_key() -> String {
    use rand::RngCore;
    let mut buf = [0u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut buf);
    format!("sk_{}", hex::encode(buf))
}
