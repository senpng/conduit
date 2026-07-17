use conduit_ir::{
    canonical::{
        CanonicalChatRequest, CanonicalContent, CanonicalMessage, RequestMeta, Role, Sampling,
        ToolChoice, ToolDef,
    },
    error::CodecError,
};
use serde_json::Value;

/// Decode an Anthropic Messages API request body into the canonical IR.
pub fn decode_request(
    body: Value,
    alias: String,
    stream: bool,
    request_id: String,
    _key_id: String,
) -> Result<CanonicalChatRequest, CodecError> {
    let msgs_raw = body["messages"]
        .as_array()
        .ok_or_else(|| CodecError::MissingField {
            field: "messages".into(),
        })?;

    let mut messages: Vec<CanonicalMessage> = Vec::new();

    // System field (string or content block array) → synthetic System message.
    if let Some(system_content) = parse_system_field(&body["system"]) {
        messages.push(CanonicalMessage {
            role: Role::System,
            content: system_content,
            name: None,
        });
    }

    for msg in msgs_raw {
        let role = match msg["role"].as_str().unwrap_or("user") {
            "assistant" => Role::Assistant,
            _ => Role::User,
        };
        let content = parse_content(&msg["content"])?;
        messages.push(CanonicalMessage {
            role,
            content,
            name: None,
        });
    }

    let tools: Vec<ToolDef> = body["tools"]
        .as_array()
        .unwrap_or(&vec![])
        .iter()
        .filter_map(|t| {
            let name = t["name"].as_str()?.to_string();
            let description = t["description"].as_str().map(String::from);
            let parameters = t["input_schema"].clone();
            Some(ToolDef {
                name,
                description,
                parameters,
            })
        })
        .collect();

    let tool_choice = decode_tool_choice(&body["tool_choice"]);

    let mut sampling = Sampling {
        temperature: body["temperature"].as_f64().map(|v| v as f32),
        top_p: body["top_p"].as_f64().map(|v| v as f32),
        top_k: body["top_k"].as_u64().map(|v| v as u32),
        max_tokens: body["max_tokens"].as_u64().map(|v| v as u32),
        stop: body["stop_sequences"].as_array().map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        }),
        ..Default::default()
    };

    // Claude thinking → OpenAI reasoning_effort (CLIProxyAPI ConvertBudgetToLevel).
    if let Some(effort) = decode_thinking_to_reasoning_effort(&body) {
        sampling.reasoning_effort = Some(effort);
    }

    let mut meta = RequestMeta {
        user: body["user"].as_str().map(String::from),
        extra: Default::default(),
    };
    // Preserve raw thinking config for providers that understand it natively.
    if !body["thinking"].is_null() {
        meta.extra
            .insert("thinking".into(), body["thinking"].clone());
    }

    Ok(CanonicalChatRequest {
        id: request_id.clone(),
        alias,
        messages,
        tools,
        tool_choice,
        response_format: None,
        sampling,
        meta,
        stream,
        loss_report: Default::default(),
    })
}

/// Map Anthropic `thinking` object to OpenAI `reasoning_effort`.
fn decode_thinking_to_reasoning_effort(body: &Value) -> Option<String> {
    let thinking = body.get("thinking")?;
    if thinking.is_null() {
        return None;
    }
    let ty = thinking.get("type")?.as_str()?;
    match ty {
        "enabled" => {
            if let Some(budget) = thinking.get("budget_tokens").and_then(|b| b.as_i64()) {
                Some(budget_to_effort(budget as i32).to_string())
            } else {
                Some("auto".into())
            }
        }
        "adaptive" | "auto" => {
            // Claude 4.6 may put effort in output_config.effort.
            if let Some(effort) = body
                .pointer("/output_config/effort")
                .and_then(|v| v.as_str())
            {
                let e = effort.trim().to_ascii_lowercase();
                if !e.is_empty() {
                    return Some(e);
                }
            }
            Some("xhigh".into())
        }
        "disabled" => Some("none".into()),
        _ => None,
    }
}

/// CLIProxyAPI `ConvertBudgetToLevel` thresholds.
fn budget_to_effort(budget: i32) -> &'static str {
    match budget {
        i if i < -1 => "auto",
        -1 => "auto",
        0 => "none",
        1..=512 => "minimal",
        513..=1024 => "low",
        1025..=8192 => "medium",
        8193..=24576 => "high",
        _ => "xhigh",
    }
}

fn parse_system_field(val: &Value) -> Option<Vec<CanonicalContent>> {
    if val.is_null() {
        return None;
    }
    if let Some(text) = val.as_str() {
        if text.is_empty() {
            return None;
        }
        return Some(vec![CanonicalContent::Text {
            text: text.to_string(),
        }]);
    }
    if let Some(arr) = val.as_array() {
        let parts = parse_content_array(arr).ok()?;
        if parts.is_empty() {
            None
        } else {
            Some(parts)
        }
    } else {
        None
    }
}

pub(crate) fn parse_content(val: &Value) -> Result<Vec<CanonicalContent>, CodecError> {
    if let Some(text) = val.as_str() {
        return Ok(vec![CanonicalContent::Text {
            text: text.to_string(),
        }]);
    }
    if let Some(arr) = val.as_array() {
        return parse_content_array(arr);
    }
    Ok(vec![])
}

fn parse_content_array(arr: &[Value]) -> Result<Vec<CanonicalContent>, CodecError> {
    let mut out = Vec::new();
    for block in arr {
        match block["type"].as_str() {
            Some("text") => {
                out.push(CanonicalContent::Text {
                    text: block["text"].as_str().unwrap_or("").to_string(),
                });
            }
            Some("image") => {
                let source = &block["source"];
                let media_type = source["media_type"].as_str().map(String::from);
                match source["type"].as_str() {
                    Some("url") => {
                        let url = source["url"].as_str().unwrap_or("").to_string();
                        out.push(CanonicalContent::Image {
                            url,
                            media_type,
                            detail: None,
                        });
                    }
                    Some("base64") => {
                        let b64 = source["data"].as_str().unwrap_or("");
                        let data_url = format!(
                            "data:{};base64,{}",
                            media_type.as_deref().unwrap_or("image/jpeg"),
                            b64
                        );
                        out.push(CanonicalContent::Image {
                            url: data_url,
                            media_type,
                            detail: None,
                        });
                    }
                    _ => {}
                }
            }
            Some("tool_use") => {
                let id = block["id"].as_str().unwrap_or("").to_string();
                let name = block["name"].as_str().unwrap_or("").to_string();
                let input = block["input"].clone();
                out.push(CanonicalContent::ToolUse { id, name, input });
            }
            Some("tool_result") => {
                let tool_use_id = block["tool_use_id"].as_str().unwrap_or("").to_string();
                let is_error = block["is_error"].as_bool();
                let content = if let Some(arr) = block["content"].as_array() {
                    parse_content_array(arr)?
                } else if let Some(text) = block["content"].as_str() {
                    vec![CanonicalContent::Text {
                        text: text.to_string(),
                    }]
                } else {
                    vec![]
                };
                out.push(CanonicalContent::ToolResult {
                    tool_use_id,
                    content,
                    is_error,
                });
            }
            Some("thinking") => {
                let thinking = block["thinking"].as_str().unwrap_or("").to_string();
                let signature = block["signature"].as_str().map(String::from);
                out.push(CanonicalContent::Thinking {
                    thinking,
                    signature,
                });
            }
            _ => {}
        }
    }
    Ok(out)
}

fn decode_tool_choice(val: &Value) -> Option<ToolChoice> {
    if val.is_null() {
        return None;
    }
    match val["type"].as_str() {
        Some("auto") => Some(ToolChoice::Auto),
        Some("any") => Some(ToolChoice::Required),
        Some("tool") => val["name"].as_str().map(|n| ToolChoice::Tool {
            name: n.to_string(),
        }),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn decode(body: Value) -> CanonicalChatRequest {
        decode_request(
            body,
            "claude-3-5-sonnet".into(),
            false,
            "req-1".into(),
            "key-1".into(),
        )
        .unwrap()
    }

    #[test]
    fn user_message() {
        let req = decode(json!({"messages": [{"role": "user", "content": "Hi"}]}));
        assert_eq!(req.messages[0].role, Role::User);
    }

    #[test]
    fn system_string_decoded() {
        let req = decode(json!({
            "system": "Be helpful",
            "messages": [{"role": "user", "content": "Hi"}]
        }));
        assert_eq!(req.messages[0].role, Role::System);
    }

    #[test]
    fn tool_use_in_assistant_decoded() {
        let body = json!({
            "messages": [{
                "role": "assistant",
                "content": [
                    {"type": "tool_use", "id": "tu_1", "name": "search", "input": {"q": "rust"}}
                ]
            }]
        });
        let req = decode(body);
        if let CanonicalContent::ToolUse { id, .. } = &req.messages[0].content[0] {
            assert_eq!(id, "tu_1");
        } else {
            panic!("expected ToolUse");
        }
    }

    #[test]
    fn tool_result_is_error_preserved() {
        let body = json!({
            "messages": [{
                "role": "user",
                "content": [{
                    "type": "tool_result",
                    "tool_use_id": "tu_1",
                    "is_error": true,
                    "content": "Failed"
                }]
            }]
        });
        let req = decode(body);
        if let CanonicalContent::ToolResult { is_error, .. } = &req.messages[0].content[0] {
            assert_eq!(*is_error, Some(true));
        } else {
            panic!("expected ToolResult");
        }
    }

    #[test]
    fn thinking_enabled_maps_to_reasoning_effort() {
        let body = json!({
            "thinking": {"type": "enabled", "budget_tokens": 10000},
            "messages": [{"role": "user", "content": "Hi"}]
        });
        let req = decode(body);
        assert_eq!(
            req.sampling.reasoning_effort.as_deref(),
            Some("high"),
            "budget 10000 → high (CLIProxyAPI ConvertBudgetToLevel)"
        );
    }

    #[test]
    fn thinking_disabled_maps_to_none() {
        let body = json!({
            "thinking": {"type": "disabled"},
            "messages": [{"role": "user", "content": "Hi"}]
        });
        let req = decode(body);
        assert_eq!(req.sampling.reasoning_effort.as_deref(), Some("none"));
    }
}
