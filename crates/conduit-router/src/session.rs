//! Extract a **session identity** for affinity pinning.
//!
//! Sources (first non-empty wins), aligned with CLIProxyAPI session affinity:
//! 1. `metadata.user_id` Claude Code forms:
//!    - string suffix `_session_{id}`
//!    - JSON object / JSON-string with `session_id`
//! 2. Header `X-Claude-Code-Session-Id`
//! 3. Generic session headers: `X-Session-ID`, `Session-Id`, `session_id`
//! 4. Body `conversation_id` / top-level `session_id`
//! 5. Non-Claude bare `metadata.user_id` (weak sticky key)
//! 6. `metadata.session_id`
//! 7. Header `X-Client-Request-Id` (request-scoped; last among headers)
//! 8. Body `previous_response_id`
//!
//! When nothing is found, affinity must not pin.

use serde_json::Value;

/// Extract session id from request headers and optional JSON body.
///
/// `headers` are `(name, value)` pairs; names are matched case-insensitively.
/// `body` may be the raw JSON request object (or any Value containing the fields).
pub fn extract_session_id(
    headers: &[(String, String)],
    body: Option<&Value>,
) -> Option<String> {
    // 1. Claude Code identity from metadata.user_id (highest — CLIProxyAPI parity).
    //    Accept string (classic / JSON-string) and object forms.
    if let Some(sess) = body
        .and_then(|b| b.get("metadata"))
        .and_then(|m| m.get("user_id"))
        .and_then(claude_code_session_from_user_id_value)
    {
        return Some(sess);
    }

    // 2. Claude Code session header (conversation-scoped, not generic sticky).
    if let Some(s) = header_value(headers, "x-claude-code-session-id") {
        return Some(s);
    }

    // 3. Generic session headers.
    for name in ["x-session-id", "session-id", "session_id"] {
        if let Some(s) = header_value(headers, name) {
            return Some(s);
        }
    }

    let body = body?;

    // 4. Explicit conversation / session fields.
    if let Some(s) = string_field(body, "conversation_id") {
        return Some(s);
    }
    if let Some(s) = string_field(body, "session_id") {
        return Some(s);
    }

    // 5–6. Metadata fallbacks (bare user_id only after Claude-shaped parse failed).
    if let Some(meta) = body.get("metadata") {
        if let Some(uid) = string_field(meta, "user_id") {
            // Non-Claude-Code user_id still scopes affinity when present.
            if !uid.is_empty() {
                return Some(uid);
            }
        }
        if let Some(s) = string_field(meta, "session_id") {
            return Some(s);
        }
    }

    // 7. Per-request client id (weak for multi-turn; after stronger signals).
    if let Some(s) = header_value(headers, "x-client-request-id") {
        return Some(s);
    }

    // 8. Responses continuation cursor (last resort).
    if let Some(s) = string_field(body, "previous_response_id") {
        return Some(s);
    }
    None
}

fn header_value(headers: &[(String, String)], want: &str) -> Option<String> {
    for (name, value) in headers {
        if name.trim().eq_ignore_ascii_case(want) {
            let v = value.trim();
            if !v.is_empty() {
                return Some(v.to_string());
            }
        }
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

/// Claude Code `user_id` session extraction from a wire JSON value.
///
/// Supports:
/// - classic string ending with `_session_{id}`
/// - JSON-string: `"{\"device_id\":\"…\",\"session_id\":\"…\"}"`
/// - object: `{"device_id":"…","session_id":"…"}` (gjson-style object→text parity)
fn claude_code_session_from_user_id_value(user_id: &Value) -> Option<String> {
    match user_id {
        Value::String(s) => claude_code_session_from_user_id(s),
        Value::Object(obj) => obj
            .get("session_id")
            .and_then(|s| s.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string()),
        _ => None,
    }
}

/// Claude Code `user_id` session extraction from a string field.
///
/// Supports:
/// - string form ending with `_session_{id}` (last marker wins)
/// - JSON object string: `{"session_id":"..."}` (and related fields)
fn claude_code_session_from_user_id(user_id: &str) -> Option<String> {
    let user_id = user_id.trim();
    if user_id.is_empty() {
        return None;
    }

    // JSON object form (CLIProxyAPI "new format").
    if user_id.starts_with('{') {
        if let Ok(v) = serde_json::from_str::<Value>(user_id) {
            if let Some(s) = string_field(&v, "session_id") {
                return Some(s);
            }
        }
        return None;
    }

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
    fn header_x_session_id_wins_among_generic_headers() {
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
    fn claude_code_user_id_beats_x_session_id_header() {
        let headers = vec![("X-Session-ID".into(), "header-session".into())];
        let body = json!({
            "metadata": {
                "user_id": "user_deadbeef_account__session_from-user-id"
            }
        });
        assert_eq!(
            extract_session_id(&headers, Some(&body)).as_deref(),
            Some("from-user-id")
        );
    }

    #[test]
    fn claude_code_user_id_beats_claude_session_header() {
        let headers = vec![("X-Claude-Code-Session-Id".into(), "header-cc".into())];
        let body = json!({
            "metadata": {
                "user_id": "user_aaaa_account_bbbb_session_body-session"
            }
        });
        assert_eq!(
            extract_session_id(&headers, Some(&body)).as_deref(),
            Some("body-session")
        );
    }

    #[test]
    fn claude_session_header_beats_generic_x_session_id() {
        let headers = vec![
            ("X-Session-ID".into(), "generic".into()),
            ("X-Claude-Code-Session-Id".into(), "cc-header".into()),
        ];
        assert_eq!(
            extract_session_id(&headers, None).as_deref(),
            Some("cc-header")
        );
    }

    #[test]
    fn json_user_id_session_id_field() {
        let body = json!({
            "metadata": {
                "user_id": "{\"device_id\":\"d1\",\"account_uuid\":\"\",\"session_id\":\"json-sess-1\"}"
            }
        });
        assert_eq!(
            extract_session_id(&[], Some(&body)).as_deref(),
            Some("json-sess-1")
        );
    }

    #[test]
    fn object_user_id_session_id_field() {
        // Rare: client sends metadata.user_id as a real JSON object (not stringified).
        let body = json!({
            "metadata": {
                "user_id": {
                    "device_id": "d1",
                    "account_uuid": "",
                    "session_id": "obj-sess-1"
                }
            }
        });
        assert_eq!(
            extract_session_id(&[], Some(&body)).as_deref(),
            Some("obj-sess-1")
        );
    }

    #[test]
    fn json_user_id_beats_x_session_id_header() {
        let headers = vec![("X-Session-ID".into(), "header-session".into())];
        let body = json!({
            "metadata": {
                "user_id": "{\"session_id\":\"from-json-user-id\"}"
            }
        });
        assert_eq!(
            extract_session_id(&headers, Some(&body)).as_deref(),
            Some("from-json-user-id")
        );
    }

    #[test]
    fn bare_user_id_after_claude_header_only_when_no_claude_session() {
        let headers = vec![("X-Session-ID".into(), "header-sess".into())];
        let body = json!({
            "metadata": {
                "user_id": "plain-user-without-session-marker"
            }
        });
        // Bare (non-Claude) user_id is weaker than generic session headers.
        assert_eq!(
            extract_session_id(&headers, Some(&body)).as_deref(),
            Some("header-sess")
        );
    }

    #[test]
    fn bare_user_id_used_when_no_stronger_signal() {
        let body = json!({
            "metadata": {
                "user_id": "plain-user-without-session-marker"
            }
        });
        assert_eq!(
            extract_session_id(&[], Some(&body)).as_deref(),
            Some("plain-user-without-session-marker")
        );
    }

    #[test]
    fn client_request_id_after_conversation_id() {
        let headers = vec![("X-Client-Request-Id".into(), "req-1".into())];
        let body = json!({"conversation_id": "conv-1"});
        assert_eq!(
            extract_session_id(&headers, Some(&body)).as_deref(),
            Some("conv-1")
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
