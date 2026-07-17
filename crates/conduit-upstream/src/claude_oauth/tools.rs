//! OAuth tool-name remapping (CLIProxyAPI parity).

use std::collections::HashMap;

use serde_json::{json, Value};

/// OpenCode-style → Claude Code TitleCase tool names.
const OAUTH_TOOL_RENAME: &[(&str, &str)] = &[
    ("bash", "Bash"),
    ("read", "Read"),
    ("write", "Write"),
    ("edit", "Edit"),
    ("glob", "Glob"),
    ("grep", "Grep"),
    ("task", "Task"),
    ("webfetch", "WebFetch"),
    ("todowrite", "TodoWrite"),
    ("question", "Question"),
    ("skill", "Skill"),
    ("ls", "LS"),
    ("todoread", "TodoRead"),
    ("notebookedit", "NotebookEdit"),
];

fn rename_lookup(name: &str) -> Option<&'static str> {
    OAUTH_TOOL_RENAME
        .iter()
        .find(|(from, _)| *from == name)
        .map(|(_, to)| *to)
}

/// Remap tool names for OAuth upstream. Returns body + reverse map (upstream → original).
pub fn remap_oauth_tool_names(mut body: Value) -> (Value, HashMap<String, String>) {
    let mut reverse: HashMap<String, String> = HashMap::new();
    let mut record = |original: String, renamed: String| {
        reverse.entry(renamed).or_insert(original);
    };

    // 1. tools[]
    if let Some(tools) = body.get_mut("tools").and_then(|t| t.as_array_mut()) {
        for tool in tools.iter_mut() {
            // Built-in tools have a non-empty type field (web_search_*, etc.)
            if tool
                .get("type")
                .and_then(|t| t.as_str())
                .map(|s| !s.is_empty())
                .unwrap_or(false)
            {
                continue;
            }
            if let Some(name) = tool
                .get("name")
                .and_then(|n| n.as_str())
                .map(|s| s.to_string())
            {
                if let Some(new_name) = rename_lookup(&name) {
                    if new_name != name {
                        tool["name"] = json!(new_name);
                        record(name, new_name.to_string());
                    }
                }
            }
        }
    }

    // 2. tool_choice
    if body.pointer("/tool_choice/type").and_then(|t| t.as_str()) == Some("tool") {
        if let Some(tc_name) = body
            .pointer("/tool_choice/name")
            .and_then(|n| n.as_str())
            .map(|s| s.to_string())
        {
            if let Some(new_name) = rename_lookup(&tc_name) {
                if new_name != tc_name {
                    if let Some(slot) = body.pointer_mut("/tool_choice/name") {
                        *slot = json!(new_name);
                    }
                    record(tc_name, new_name.to_string());
                }
            }
        }
    }

    // 3. messages content tool_use / tool_reference
    if let Some(messages) = body.get_mut("messages").and_then(|m| m.as_array_mut()) {
        for msg in messages.iter_mut() {
            let Some(content) = msg.get_mut("content").and_then(|c| c.as_array_mut()) else {
                continue;
            };
            for part in content.iter_mut() {
                let part_type = part.get("type").and_then(|t| t.as_str()).unwrap_or("");
                match part_type {
                    "tool_use" => {
                        if let Some(name) = part
                            .get("name")
                            .and_then(|n| n.as_str())
                            .map(|s| s.to_string())
                        {
                            if let Some(new_name) = rename_lookup(&name) {
                                if new_name != name {
                                    part["name"] = json!(new_name);
                                    record(name, new_name.to_string());
                                }
                            }
                        }
                    }
                    "tool_reference" => {
                        if let Some(name) = part
                            .get("tool_name")
                            .and_then(|n| n.as_str())
                            .map(|s| s.to_string())
                        {
                            if let Some(new_name) = rename_lookup(&name) {
                                if new_name != name {
                                    part["tool_name"] = json!(new_name);
                                    record(name, new_name.to_string());
                                }
                            }
                        }
                    }
                    "tool_result" => {
                        if let Some(nested) = part.get_mut("content").and_then(|c| c.as_array_mut())
                        {
                            for nested_part in nested.iter_mut() {
                                if nested_part.get("type").and_then(|t| t.as_str())
                                    == Some("tool_reference")
                                {
                                    if let Some(name) = nested_part
                                        .get("tool_name")
                                        .and_then(|n| n.as_str())
                                        .map(|s| s.to_string())
                                    {
                                        if let Some(new_name) = rename_lookup(&name) {
                                            if new_name != name {
                                                nested_part["tool_name"] = json!(new_name);
                                                record(name, new_name.to_string());
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
    }

    (body, reverse)
}

/// Restore tool names in a non-stream Anthropic response `content[]`.
pub fn reverse_remap_response(mut body: Value, reverse: &HashMap<String, String>) -> Value {
    if reverse.is_empty() {
        return body;
    }
    if let Some(content) = body.get_mut("content").and_then(|c| c.as_array_mut()) {
        for part in content.iter_mut() {
            let part_type = part.get("type").and_then(|t| t.as_str()).unwrap_or("");
            match part_type {
                "tool_use" => {
                    if let Some(name) = part.get("name").and_then(|n| n.as_str()) {
                        if let Some(orig) = reverse.get(name) {
                            part["name"] = json!(orig);
                        }
                    }
                }
                "tool_reference" => {
                    if let Some(name) = part.get("tool_name").and_then(|n| n.as_str()) {
                        if let Some(orig) = reverse.get(name) {
                            part["tool_name"] = json!(orig);
                        }
                    }
                }
                _ => {}
            }
        }
    }
    body
}

/// Restore tool names in an SSE event JSON payload (content_block).
pub fn reverse_remap_stream_payload(
    mut payload: Value,
    reverse: &HashMap<String, String>,
) -> Value {
    if reverse.is_empty() {
        return payload;
    }
    let Some(block) = payload.get("content_block") else {
        return payload;
    };
    let block_type = block.get("type").and_then(|t| t.as_str()).unwrap_or("");
    match block_type {
        "tool_use" => {
            if let Some(name) = block.get("name").and_then(|n| n.as_str()) {
                if let Some(orig) = reverse.get(name) {
                    if let Some(slot) = payload.pointer_mut("/content_block/name") {
                        *slot = json!(orig);
                    }
                }
            }
        }
        "tool_reference" => {
            if let Some(name) = block.get("tool_name").and_then(|n| n.as_str()) {
                if let Some(orig) = reverse.get(name) {
                    if let Some(slot) = payload.pointer_mut("/content_block/tool_name") {
                        *slot = json!(orig);
                    }
                }
            }
        }
        _ => {}
    }
    payload
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn remaps_and_restores_tool_names() {
        let body = json!({
            "tools": [{"name": "bash", "input_schema": {"type": "object"}}],
            "messages": [{
                "role": "assistant",
                "content": [{"type": "tool_use", "id": "1", "name": "glob", "input": {}}]
            }]
        });
        let (out, rev) = remap_oauth_tool_names(body);
        assert_eq!(out["tools"][0]["name"], "Bash");
        assert_eq!(out["messages"][0]["content"][0]["name"], "Glob");
        assert_eq!(rev.get("Bash").map(String::as_str), Some("bash"));
        assert_eq!(rev.get("Glob").map(String::as_str), Some("glob"));

        let resp = json!({
            "content": [{"type": "tool_use", "name": "Bash", "id": "x", "input": {}}]
        });
        let restored = reverse_remap_response(resp, &rev);
        assert_eq!(restored["content"][0]["name"], "bash");
    }
}
