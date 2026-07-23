//! Claude Code system cloak + billing header (CLIProxyAPI parity).

use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use super::{
    prompts::{self, AGENT_IDENTIFIER, SANITIZED_SYSTEM_REMINDER},
    session::{
        cached_user_id, generate_fake_user_id, should_preserve_user_id,
    },
};

const FINGERPRINT_SALT: &str = "59cf53e54c78";
pub const DEFAULT_CLAUDE_VERSION: &str = "2.1.63";

/// Salted 3-char build fingerprint for `cc_version`.
fn compute_fingerprint(message_text: &str, version: &str) -> String {
    let indices = [4usize, 7, 20];
    let runes: Vec<char> = message_text.chars().collect();
    let mut picked = String::new();
    for idx in indices {
        if idx < runes.len() {
            picked.push(runes[idx]);
        } else {
            picked.push('0');
        }
    }
    let input = format!("{FINGERPRINT_SALT}{picked}{version}");
    let h = Sha256::digest(input.as_bytes());
    hex::encode(&h[..])[..3].to_string()
}

/// Billing header with `cch=00000;` placeholder for OAuth cch signing.
pub fn generate_billing_header(version: &str, message_text: &str, entrypoint: &str) -> String {
    let entrypoint = if entrypoint.is_empty() {
        "cli"
    } else {
        entrypoint
    };
    let build_hash = compute_fingerprint(message_text, version);
    format!(
        "x-anthropic-billing-header: cc_version={version}.{build_hash}; cc_entrypoint={entrypoint}; cch=00000;"
    )
}

fn first_system_text(system: &Value) -> String {
    match system {
        Value::Array(arr) => {
            for part in arr {
                if part.get("type").and_then(|t| t.as_str()) == Some("text") {
                    if let Some(t) = part.get("text").and_then(|t| t.as_str()) {
                        return t.to_string();
                    }
                }
            }
            String::new()
        }
        Value::String(s) => s.clone(),
        _ => String::new(),
    }
}

fn collect_system_texts(system: &Value) -> Vec<String> {
    match system {
        Value::Array(arr) => arr
            .iter()
            .filter_map(|part| {
                if part.get("type").and_then(|t| t.as_str()) == Some("text") {
                    part.get("text")
                        .and_then(|t| t.as_str())
                        .map(|s| s.trim().to_string())
                        .filter(|s| !s.is_empty())
                } else {
                    None
                }
            })
            .collect(),
        Value::String(s) => {
            let t = s.trim();
            if t.is_empty() {
                vec![]
            } else {
                vec![t.to_string()]
            }
        }
        _ => vec![],
    }
}

fn text_block(text: &str) -> Value {
    json!({"type": "text", "text": text})
}

fn prepend_to_first_user_message(mut body: Value, text: &str) -> Value {
    let Some(messages) = body.get_mut("messages").and_then(|m| m.as_array_mut()) else {
        return body;
    };
    let first_user_idx = messages
        .iter()
        .position(|m| m.get("role").and_then(|r| r.as_str()) == Some("user"));
    let Some(idx) = first_user_idx else {
        return body;
    };

    let prefix = format!(
        r#"<system-reminder>
As you answer the user's questions, you can use the following context from the system:
{text}

IMPORTANT: this context may or may not be relevant to your tasks. You should not respond to this context unless it is highly relevant to your task.
</system-reminder>
"#
    );

    let content = &mut messages[idx]["content"];
    if let Some(arr) = content.as_array_mut() {
        arr.insert(0, text_block(&prefix));
    } else if let Some(s) = content.as_str() {
        *content = json!(format!("{prefix}{s}"));
    } else if content.is_null() {
        *content = json!([text_block(&prefix)]);
    }
    body
}

/// Inject Claude Code system structure for OAuth (billing + agent + static prompts).
///
/// - `strict_mode`: keep original system texts in system[] (no move to user)
/// - otherwise: sanitize third-party system into neutral reminder on first user msg
pub fn apply_oauth_system_cloak(
    mut body: Value,
    model: &str,
    version: &str,
    entrypoint: &str,
    strict_mode: bool,
    oauth_mode: bool,
) -> Value {
    if model.starts_with("claude-3-5-haiku") {
        return body;
    }

    let system = body.get("system").cloned().unwrap_or(Value::Null);
    // Skip if already injected
    if first_system_text(&system).starts_with("x-anthropic-billing-header:") {
        return body;
    }

    let message_text = first_system_text(&system);
    let billing = generate_billing_header(version, &message_text, entrypoint);
    let static_prompt = prompts::static_claude_code_prompt();

    body["system"] = json!([
        text_block(&billing),
        text_block(AGENT_IDENTIFIER),
        text_block(&static_prompt),
    ]);

    if !strict_mode {
        let user_parts = collect_system_texts(&system);
        if !user_parts.is_empty() {
            let combined = if oauth_mode {
                // OAuth: collapse third-party system into neutral reminder
                SANITIZED_SYSTEM_REMINDER.to_string()
            } else {
                user_parts.join("\n\n")
            };
            if !combined.trim().is_empty() {
                body = prepend_to_first_user_message(body, &combined);
            }
        }
    }

    body
}

pub fn inject_fake_user_id(mut body: Value, api_key: &str, use_cache: bool) -> Value {
    // Preserve client Claude Code identity when present:
    // - classic `user_…_account_…_session_…`
    // - JSON-string `{"device_id","account_uuid","session_id"}` (new Claude Code)
    // - rare object form of the same fields
    if let Some(existing) = body.pointer("/metadata/user_id") {
        if should_preserve_user_id(existing) {
            return body;
        }
    }
    let user_id = if use_cache {
        cached_user_id(api_key)
    } else {
        generate_fake_user_id()
    };
    if body.get("metadata").is_none() {
        body["metadata"] = json!({});
    }
    body["metadata"]["user_id"] = json!(user_id);
    body
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn cloak_injects_billing_and_moves_system() {
        let body = json!({
            "system": "You are a custom agent",
            "messages": [{"role": "user", "content": "hi"}]
        });
        let out = apply_oauth_system_cloak(
            body,
            "claude-sonnet-4",
            DEFAULT_CLAUDE_VERSION,
            "cli",
            false,
            true,
        );
        let sys = out["system"].as_array().unwrap();
        assert_eq!(sys.len(), 3);
        assert!(sys[0]["text"]
            .as_str()
            .unwrap()
            .starts_with("x-anthropic-billing-header:"));
        assert_eq!(sys[1]["text"], AGENT_IDENTIFIER);
        // String content stays string (CLIProxyAPI prepends text).
        let user_content = out["messages"][0]["content"].as_str().unwrap();
        assert!(user_content.contains("system-reminder"));
        assert!(user_content.contains("hi") || user_content.ends_with("hi"));
    }

    #[test]
    fn inject_preserves_json_string_user_id() {
        let client_uid = r#"{"device_id":"be82c3aee1e0c2d74535bacc85f9f559228f02dd8a17298cf522b71e6c375714","account_uuid":"","session_id":"e26d4046-0f88-4b09-bb5b-f863ab5fb24e"}"#;
        let body = json!({
            "metadata": {"user_id": client_uid},
            "messages": [{"role": "user", "content": "hi"}]
        });
        let out = inject_fake_user_id(body, "sk-ant-oat-test", true);
        assert_eq!(
            out["metadata"]["user_id"].as_str().unwrap(),
            client_uid,
            "JSON-string user_id must not be overwritten by fake classic id"
        );
    }

    #[test]
    fn inject_preserves_classic_user_id() {
        let client_uid = "user_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa_account_11111111-1111-1111-1111-111111111111_session_22222222-2222-2222-2222-222222222222";
        let body = json!({
            "metadata": {"user_id": client_uid}
        });
        let out = inject_fake_user_id(body, "sk-ant-oat-test", true);
        assert_eq!(out["metadata"]["user_id"].as_str().unwrap(), client_uid);
    }

    #[test]
    fn inject_preserves_object_user_id() {
        let body = json!({
            "metadata": {
                "user_id": {
                    "device_id": "d1",
                    "account_uuid": "",
                    "session_id": "obj-sess-1"
                }
            }
        });
        let out = inject_fake_user_id(body, "sk-ant-oat-test", true);
        assert_eq!(out["metadata"]["user_id"]["session_id"], "obj-sess-1");
        assert_eq!(out["metadata"]["user_id"]["device_id"], "d1");
    }

    #[test]
    fn inject_replaces_invalid_user_id() {
        let body = json!({
            "metadata": {"user_id": "not-a-claude-id"}
        });
        let out = inject_fake_user_id(body, "sk-ant-oat-replace", true);
        let uid = out["metadata"]["user_id"].as_str().unwrap();
        assert!(uid.starts_with("user_"), "uid={uid}");
        assert!(uid.contains("_session_"), "uid={uid}");
    }
}
