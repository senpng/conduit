//! Extract a **session identity** for affinity pinning.
//!
//! Sources (first non-empty wins), aligned with common coding clients / CLIProxyAPI:
//! 1. Headers: `X-Session-ID`, `Session-Id`, `Session_id`, `X-Client-Request-Id`
//! 2. Body JSON fields: `conversation_id`, `session_id`, `metadata.user_id` (Claude Code
//!    `_session_{uuid}` suffix when present), `previous_response_id`
//!
//! When nothing is found, affinity must not pin.

use serde_json::Value;

/// Header names checked case-insensitively (order is priority).
const SESSION_HEADERS: &[&str] = &[
    "x-session-id",
    "session-id",
    "session_id",
    "x-claude-code-session-id",
    "x-client-request-id",
];

/// Extract session id from request headers and optional JSON body.
///
/// `headers` are `(name, value)` pairs; names are matched case-insensitively.
/// `body` may be the raw JSON request object (or any Value containing the fields).
pub fn extract_session_id(
    headers: &[(String, String)],
    body: Option<&Value>,
) -> Option<String> {
    for (name, value) in headers {
        let key = name.trim().to_ascii_lowercase();
        if SESSION_HEADERS.iter().any(|h| *h == key) {
            let v = value.trim();
            if !v.is_empty() {
                return Some(v.to_string());
            }
        }
    }

    let body = body?;
    if let Some(s) = string_field(body, "conversation_id") {
        return Some(s);
    }
    if let Some(s) = string_field(body, "session_id") {
        return Some(s);
    }
    if let Some(meta) = body.get("metadata") {
        if let Some(uid) = string_field(meta, "user_id") {
            if let Some(sess) = claude_code_session_from_user_id(&uid) {
                return Some(sess);
            }
            // Non-Claude-Code user_id still scopes affinity when present.
            if !uid.is_empty() {
                return Some(uid);
            }
        }
        if let Some(s) = string_field(meta, "session_id") {
            return Some(s);
        }
    }
    if let Some(s) = string_field(body, "previous_response_id") {
        return Some(s);
    }
    None
}

fn string_field(v: &Value, key: &str) -> Option<String> {
    v.get(key)
        .and_then(|x| x.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
}

/// Claude Code `user_id` often ends with `_session_{uuid}`.
fn claude_code_session_from_user_id(user_id: &str) -> Option<String> {
    const MARKER: &str = "_session_";
    if let Some(idx) = user_id.rfind(MARKER) {
        let rest = &user_id[idx + MARKER.len()..];
        let sess = rest.trim();
        if !sess.is_empty() {
            return Some(sess.to_string());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn header_x_session_id_wins() {
        let headers = vec![
            ("X-Session-ID".into(), "sess-abc".into()),
            ("Session-Id".into(), "other".into()),
        ];
        assert_eq!(
            extract_session_id(&headers, None).as_deref(),
            Some("sess-abc")
        );
    }

    #[test]
    fn header_claude_code_session_id() {
        let headers = vec![("X-Claude-Code-Session-Id".into(), "cc-sess".into())];
        assert_eq!(
            extract_session_id(&headers, None).as_deref(),
            Some("cc-sess")
        );
    }

    #[test]
    fn conversation_id_from_body() {
        let body = json!({"conversation_id": "conv-1", "model": "x"});
        assert_eq!(
            extract_session_id(&[], Some(&body)).as_deref(),
            Some("conv-1")
        );
    }

    #[test]
    fn claude_code_user_id_session_suffix() {
        let body = json!({
            "metadata": {
                "user_id": "user_deadbeef_account__session_550e8400-e29b-41d4-a716-446655440000"
            }
        });
        assert_eq!(
            extract_session_id(&[], Some(&body)).as_deref(),
            Some("550e8400-e29b-41d4-a716-446655440000")
        );
    }

    #[test]
    fn previous_response_id_fallback() {
        let body = json!({"previous_response_id": "resp_123"});
        assert_eq!(
            extract_session_id(&[], Some(&body)).as_deref(),
            Some("resp_123")
        );
    }

    #[test]
    fn no_session_returns_none() {
        let body = json!({"model": "gpt-4o", "messages": []});
        assert_eq!(extract_session_id(&[], Some(&body)), None);
        assert_eq!(extract_session_id(&[], None), None);
    }
}
