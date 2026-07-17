//! Prompt cache_control helpers (CLIProxyAPI parity).

use serde_json::{json, Map, Value};

fn ephemeral() -> Value {
    json!({"type": "ephemeral"})
}

fn count_cache_controls(body: &Value) -> usize {
    let mut count = 0;
    if let Some(system) = body.get("system").and_then(|s| s.as_array()) {
        count += system
            .iter()
            .filter(|i| i.get("cache_control").is_some())
            .count();
    }
    if let Some(tools) = body.get("tools").and_then(|t| t.as_array()) {
        count += tools
            .iter()
            .filter(|i| i.get("cache_control").is_some())
            .count();
    }
    if let Some(messages) = body.get("messages").and_then(|m| m.as_array()) {
        for msg in messages {
            if let Some(content) = msg.get("content").and_then(|c| c.as_array()) {
                count += content
                    .iter()
                    .filter(|i| i.get("cache_control").is_some())
                    .count();
            }
        }
    }
    count
}

fn inject_tools_cache_control(mut body: Value) -> Value {
    let Some(tools) = body.get_mut("tools").and_then(|t| t.as_array_mut()) else {
        return body;
    };
    if tools.is_empty() {
        return body;
    }
    if tools.iter().any(|t| t.get("cache_control").is_some()) {
        return body;
    }
    let last = tools.len() - 1;
    tools[last]["cache_control"] = ephemeral();
    body
}

fn inject_system_cache_control(mut body: Value) -> Value {
    match body.get("system") {
        Some(Value::Array(arr)) if !arr.is_empty() => {
            if arr.iter().any(|i| i.get("cache_control").is_some()) {
                return body;
            }
            let last = arr.len() - 1;
            if let Some(system) = body.get_mut("system").and_then(|s| s.as_array_mut()) {
                system[last]["cache_control"] = ephemeral();
            }
        }
        Some(Value::String(s)) => {
            let text = s.clone();
            body["system"] = json!([{
                "type": "text",
                "text": text,
                "cache_control": {"type": "ephemeral"}
            }]);
        }
        _ => {}
    }
    body
}

fn inject_messages_cache_control(mut body: Value) -> Value {
    let Some(messages) = body.get("messages").and_then(|m| m.as_array()) else {
        return body;
    };
    let has_any = messages.iter().any(|msg| {
        msg.get("content")
            .and_then(|c| c.as_array())
            .map(|c| c.iter().any(|i| i.get("cache_control").is_some()))
            .unwrap_or(false)
    });
    if has_any {
        return body;
    }

    let user_indices: Vec<usize> = messages
        .iter()
        .enumerate()
        .filter(|(_, m)| m.get("role").and_then(|r| r.as_str()) == Some("user"))
        .map(|(i, _)| i)
        .collect();
    if user_indices.len() < 2 {
        return body;
    }
    let target = user_indices[user_indices.len() - 2];
    let Some(messages) = body.get_mut("messages").and_then(|m| m.as_array_mut()) else {
        return body;
    };
    let content = &mut messages[target]["content"];
    if let Some(arr) = content.as_array_mut() {
        if let Some(last) = arr.last_mut() {
            last["cache_control"] = ephemeral();
        }
    } else if let Some(text) = content.as_str() {
        let t = text.to_string();
        *content = json!([{
            "type": "text",
            "text": t,
            "cache_control": {"type": "ephemeral"}
        }]);
    }
    body
}

pub fn ensure_cache_control(body: Value) -> Value {
    let body = inject_tools_cache_control(body);
    let body = inject_system_cache_control(body);
    inject_messages_cache_control(body)
}

/// Strip excess cache_control blocks (max 4). Prefer keeping last tool/system.
pub fn enforce_cache_control_limit(mut body: Value, max_blocks: usize) -> Value {
    let total = count_cache_controls(&body);
    if total <= max_blocks {
        return body;
    }
    let mut excess = total - max_blocks;

    // Phase 1: early system blocks (preserve last with cache_control)
    if let Some(system) = body.get_mut("system").and_then(|s| s.as_array_mut()) {
        let last_cc = system
            .iter()
            .rposition(|i| i.get("cache_control").is_some());
        if let Some(last) = last_cc {
            for (i, item) in system.iter_mut().enumerate() {
                if excess == 0 {
                    break;
                }
                if i == last {
                    continue;
                }
                if item.get("cache_control").is_some() {
                    if let Some(obj) = item.as_object_mut() {
                        obj.remove("cache_control");
                        excess -= 1;
                    }
                }
            }
        }
    }
    if excess == 0 {
        return body;
    }

    // Phase 2: early tools
    if let Some(tools) = body.get_mut("tools").and_then(|t| t.as_array_mut()) {
        let last_cc = tools.iter().rposition(|i| i.get("cache_control").is_some());
        if let Some(last) = last_cc {
            for (i, item) in tools.iter_mut().enumerate() {
                if excess == 0 {
                    break;
                }
                if i == last {
                    continue;
                }
                if item.get("cache_control").is_some() {
                    if let Some(obj) = item.as_object_mut() {
                        obj.remove("cache_control");
                        excess -= 1;
                    }
                }
            }
        }
    }
    if excess == 0 {
        return body;
    }

    // Phase 3: messages earliest-first
    if let Some(messages) = body.get_mut("messages").and_then(|m| m.as_array_mut()) {
        for msg in messages.iter_mut() {
            if excess == 0 {
                break;
            }
            if let Some(content) = msg.get_mut("content").and_then(|c| c.as_array_mut()) {
                for part in content.iter_mut() {
                    if excess == 0 {
                        break;
                    }
                    if part.get("cache_control").is_some() {
                        if let Some(obj) = part.as_object_mut() {
                            obj.remove("cache_control");
                            excess -= 1;
                        }
                    }
                }
            }
        }
    }
    if excess == 0 {
        return body;
    }

    // Phase 4/5: remaining system then tools
    if let Some(system) = body.get_mut("system").and_then(|s| s.as_array_mut()) {
        for item in system.iter_mut() {
            if excess == 0 {
                break;
            }
            if item.get("cache_control").is_some() {
                if let Some(obj) = item.as_object_mut() {
                    obj.remove("cache_control");
                    excess -= 1;
                }
            }
        }
    }
    if let Some(tools) = body.get_mut("tools").and_then(|t| t.as_array_mut()) {
        for item in tools.iter_mut() {
            if excess == 0 {
                break;
            }
            if item.get("cache_control").is_some() {
                if let Some(obj) = item.as_object_mut() {
                    obj.remove("cache_control");
                    excess -= 1;
                }
            }
        }
    }

    let _ = excess;
    body
}

/// Once a 5m TTL is seen, strip ttl from later 1h blocks.
pub fn normalize_cache_control_ttl(mut body: Value) -> Value {
    let mut seen_5m = false;

    let mut process = |obj: &mut Map<String, Value>| {
        let Some(cc) = obj.get("cache_control") else {
            return;
        };
        if !cc.is_object() {
            seen_5m = true;
            return;
        }
        let ttl = cc.get("ttl").and_then(|t| t.as_str());
        if ttl != Some("1h") {
            seen_5m = true;
            return;
        }
        if !seen_5m {
            return;
        }
        if let Some(cc_obj) = obj.get_mut("cache_control").and_then(|c| c.as_object_mut()) {
            cc_obj.remove("ttl");
        }
    };

    if let Some(tools) = body.get_mut("tools").and_then(|t| t.as_array_mut()) {
        for tool in tools {
            if let Some(obj) = tool.as_object_mut() {
                process(obj);
            }
        }
    }
    if let Some(system) = body.get_mut("system").and_then(|s| s.as_array_mut()) {
        for item in system {
            if let Some(obj) = item.as_object_mut() {
                process(obj);
            }
        }
    }
    if let Some(messages) = body.get_mut("messages").and_then(|m| m.as_array_mut()) {
        for msg in messages {
            if let Some(content) = msg.get_mut("content").and_then(|c| c.as_array_mut()) {
                for part in content {
                    if let Some(obj) = part.as_object_mut() {
                        process(obj);
                    }
                }
            }
        }
    }
    body
}

pub fn ensure_if_missing(body: Value) -> Value {
    if count_cache_controls(&body) == 0 {
        ensure_cache_control(body)
    } else {
        body
    }
}
