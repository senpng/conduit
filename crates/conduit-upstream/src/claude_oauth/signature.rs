//! Claude Messages history signature sanitize (CLIProxyAPI `SanitizeClaudeMessagesForClaudeUpstream`).

use base64::{engine::general_purpose::STANDARD as B64, Engine};
use serde_json::{json, Value};

const MAX_CLAUDE_THINKING_SIG_LEN: usize = 16_384;

const TOOL_USE_SIG_PATHS: &[&str] = &[
    "signature",
    "thoughtSignature",
    "thought_signature",
    "model",
];

/// Prepare history for native Claude upstream:
/// - drop invalid / foreign thinking blocks
/// - strip tool_use provenance signature fields
/// - drop messages that become empty
pub fn sanitize_claude_messages_for_claude_upstream(mut body: Value) -> Value {
    let Some(messages) = body.get_mut("messages").and_then(|m| m.as_array_mut()) else {
        return body;
    };

    let mut kept_messages: Vec<Value> = Vec::with_capacity(messages.len());
    for msg in messages.iter() {
        let Some(content) = msg.get("content").and_then(|c| c.as_array()) else {
            kept_messages.push(msg.clone());
            continue;
        };

        let mut kept_parts: Vec<Value> = Vec::with_capacity(content.len());
        for part in content {
            let part_type = part.get("type").and_then(|t| t.as_str()).unwrap_or("");
            match part_type {
                "tool_use" => {
                    kept_parts.push(strip_tool_use_signature_fields(part.clone()));
                }
                "thinking" => {
                    if should_drop_thinking_block(part) {
                        continue;
                    }
                    let mut p = part.clone();
                    if let Some(norm) = normalize_claude_signature(
                        p.get("signature").and_then(|s| s.as_str()).unwrap_or(""),
                    ) {
                        p["signature"] = json!(norm);
                    }
                    kept_parts.push(p);
                }
                _ => kept_parts.push(part.clone()),
            }
        }

        if kept_parts.is_empty() {
            // Drop empty assistant/user messages after stripping
            continue;
        }

        let mut new_msg = msg.clone();
        new_msg["content"] = Value::Array(kept_parts);
        kept_messages.push(new_msg);
    }

    body["messages"] = Value::Array(kept_messages);
    body
}

fn strip_tool_use_signature_fields(mut part: Value) -> Value {
    if let Some(obj) = part.as_object_mut() {
        for key in TOOL_USE_SIG_PATHS {
            obj.remove(*key);
        }
        // extra_content.google.thought_signature
        if let Some(extra) = obj.get_mut("extra_content").and_then(|e| e.as_object_mut()) {
            if let Some(google) = extra.get_mut("google").and_then(|g| g.as_object_mut()) {
                google.remove("thought_signature");
                google.remove("thoughtSignature");
                if google.is_empty() {
                    extra.remove("google");
                }
            }
            if extra.is_empty() {
                obj.remove("extra_content");
            }
        }
    }
    part
}

fn thinking_text(part: &Value) -> String {
    if let Some(t) = part.get("text").and_then(|t| t.as_str()) {
        return t.to_string();
    }
    match part.get("thinking") {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Object(o)) => o
            .get("text")
            .or_else(|| o.get("thinking"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        _ => String::new(),
    }
}

fn is_empty_thinking_placeholder(part: &Value) -> bool {
    let sig = part
        .get("signature")
        .and_then(|s| s.as_str())
        .unwrap_or("")
        .trim();
    sig.is_empty() && thinking_text(part).trim().is_empty()
}

fn should_drop_thinking_block(part: &Value) -> bool {
    // CLIProxyAPI DropEmptyThinkingPlaceholders=true for Claude upstream
    if is_empty_thinking_placeholder(part) {
        return true;
    }
    let raw = part.get("signature").and_then(|s| s.as_str()).unwrap_or("");
    !is_valid_claude_thinking_signature(raw)
}

fn strip_provider_prefix(raw: &str) -> String {
    let sig = raw.trim();
    if let Some(idx) = sig.find('#') {
        let (prefix, rest) = sig.split_at(idx);
        let rest = rest.trim_start_matches('#').trim();
        let p = prefix.to_ascii_lowercase();
        if matches!(
            p.as_str(),
            "claude" | "anthropic" | "gemini" | "google" | "openai" | "gpt" | "codex"
        ) {
            return rest.to_string();
        }
    }
    sig.to_string()
}

fn is_valid_claude_thinking_signature(raw: &str) -> bool {
    let sig = strip_provider_prefix(raw);
    if sig.is_empty() || sig.len() > MAX_CLAUDE_THINKING_SIG_LEN {
        return false;
    }
    if sig.contains('#') {
        return false;
    }
    match sig.as_bytes().first().copied() {
        Some(b'E') => B64
            .decode(sig.as_bytes())
            .map(|d| !d.is_empty())
            .unwrap_or(false),
        Some(b'R') => {
            let Ok(outer) = B64.decode(sig.as_bytes()) else {
                return false;
            };
            if outer.is_empty() || outer[0] != b'E' {
                return false;
            }
            B64.decode(&outer).map(|d| !d.is_empty()).unwrap_or(false)
        }
        _ => false,
    }
}

/// Strip `claude#` / `anthropic#` prefix for Claude-native E-form replay.
fn normalize_claude_signature(raw: &str) -> Option<String> {
    let stripped = strip_provider_prefix(raw);
    if stripped == raw.trim() {
        return None;
    }
    if is_valid_claude_thinking_signature(&stripped) {
        Some(stripped)
    } else {
        None
    }
}

/// Move mid-conversation `role: system` messages into top-level `system` (CLIProxyAPI optional).
pub fn rebuild_mid_system_messages_to_top_level(mut body: Value) -> Value {
    let Some(messages) = body.get("messages").and_then(|m| m.as_array()) else {
        return body;
    };

    let mut moved: Vec<Value> = Vec::new();
    let mut kept: Vec<Value> = Vec::new();
    for msg in messages {
        if msg.get("role").and_then(|r| r.as_str()) == Some("system") {
            for block in system_text_blocks(msg.get("content")) {
                moved.push(block);
            }
        } else {
            kept.push(msg.clone());
        }
    }
    if moved.is_empty() {
        return body;
    }

    let mut system_blocks = system_as_blocks(body.get("system"));
    system_blocks.extend(moved);
    body["system"] = Value::Array(system_blocks);
    body["messages"] = Value::Array(kept);
    body
}

fn system_text_blocks(content: Option<&Value>) -> Vec<Value> {
    match content {
        Some(Value::String(s)) if !s.trim().is_empty() => {
            vec![json!({"type": "text", "text": s})]
        }
        Some(Value::Array(arr)) => arr
            .iter()
            .filter(|p| p.get("type").and_then(|t| t.as_str()) == Some("text"))
            .cloned()
            .collect(),
        _ => vec![],
    }
}

fn system_as_blocks(system: Option<&Value>) -> Vec<Value> {
    match system {
        Some(Value::String(s)) if !s.trim().is_empty() => {
            vec![json!({"type": "text", "text": s})]
        }
        Some(Value::Array(arr)) => arr.clone(),
        _ => vec![],
    }
}

/// Extract body `betas` and remove the field (merged into Anthropic-Beta header).
pub fn extract_and_remove_betas(mut body: Value) -> (Vec<String>, Value) {
    let mut betas = Vec::new();
    match body.get("betas") {
        Some(Value::Array(arr)) => {
            for item in arr {
                if let Some(s) = item.as_str() {
                    let t = s.trim();
                    if !t.is_empty() {
                        betas.push(t.to_string());
                    }
                }
            }
        }
        Some(Value::String(s)) => {
            let t = s.trim();
            if !t.is_empty() {
                betas.push(t.to_string());
            }
        }
        _ => {}
    }
    if let Some(obj) = body.as_object_mut() {
        obj.remove("betas");
    }
    (betas, body)
}

pub fn ensure_model_max_tokens(mut body: Value, default_max: u64) -> Value {
    if body.get("max_tokens").is_some() {
        return body;
    }
    body["max_tokens"] = json!(default_max);
    body
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn drops_invalid_thinking_and_tool_sigs() {
        // Valid Claude E-form: whole string is std base64 and starts with 'E'.
        let e_sig = "EAB4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHg=";
        assert!(is_valid_claude_thinking_signature(e_sig));
        let body = json!({
            "messages": [
                {"role": "assistant", "content": [
                    {"type": "thinking", "thinking": "a", "signature": "gpt#not-claude"},
                    {"type": "thinking", "thinking": "b", "signature": e_sig},
                    {"type": "tool_use", "id": "1", "name": "Bash", "input": {},
                     "signature": "x", "thought_signature": "y", "extra_content": {"google": {"thought_signature": "z"}}}
                ]}
            ]
        });
        let out = sanitize_claude_messages_for_claude_upstream(body);
        let content = out["messages"][0]["content"].as_array().unwrap();
        assert_eq!(content.len(), 2);
        assert_eq!(content[0]["type"], "thinking");
        assert_eq!(content[1]["type"], "tool_use");
        assert!(content[1].get("signature").is_none());
        assert!(content[1].get("thought_signature").is_none());
        assert!(content[1].get("extra_content").is_none());
    }

    #[test]
    fn extracts_betas() {
        let body = json!({"betas": ["foo-beta", "bar"], "model": "x"});
        let (betas, out) = extract_and_remove_betas(body);
        assert_eq!(betas, vec!["foo-beta", "bar"]);
        assert!(out.get("betas").is_none());
    }
}
