use conduit_ir::{
    canonical::{
        CanonicalChatRequest, CanonicalContent, CanonicalMessage, RequestMeta, ResponseFormat,
        Role, Sampling, ToolChoice, ToolDef,
    },
    error::CodecError,
};
use serde_json::Value;

/// Decode an OpenAI `/v1/chat/completions` request body into the canonical IR.
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

    for msg in msgs_raw {
        let role_str = msg["role"].as_str().unwrap_or("user");
        match role_str {
            "system" => {
                let text = msg["content"].as_str().unwrap_or("").to_string();
                messages.push(CanonicalMessage {
                    role: Role::System,
                    content: vec![CanonicalContent::Text { text }],
                    name: msg["name"].as_str().map(String::from),
                });
            }
            "assistant" => {
                let mut content: Vec<CanonicalContent> = Vec::new();

                // Text part
                if let Some(text) = msg["content"].as_str() {
                    if !text.is_empty() {
                        content.push(CanonicalContent::Text {
                            text: text.to_string(),
                        });
                    }
                }

                // Tool calls
                if let Some(tcs) = msg["tool_calls"].as_array() {
                    for tc in tcs {
                        let id = tc["id"]
                            .as_str()
                            .ok_or_else(|| CodecError::MissingField {
                                field: "tool_calls[].id".into(),
                            })?
                            .to_string();
                        let name = tc["function"]["name"]
                            .as_str()
                            .ok_or_else(|| CodecError::MissingField {
                                field: "tool_calls[].function.name".into(),
                            })?
                            .to_string();
                        let args_str = tc["function"]["arguments"].as_str().unwrap_or("{}");
                        let input: Value = serde_json::from_str(args_str).map_err(|e| {
                            CodecError::InvalidValue {
                                field: "tool_calls[].function.arguments".into(),
                                value: e.to_string(),
                            }
                        })?;
                        content.push(CanonicalContent::ToolUse { id, name, input });
                    }
                }

                messages.push(CanonicalMessage {
                    role: Role::Assistant,
                    content,
                    name: msg["name"].as_str().map(String::from),
                });
            }
            "tool" => {
                let tool_call_id = msg["tool_call_id"]
                    .as_str()
                    .ok_or_else(|| CodecError::MissingField {
                        field: "tool_call_id".into(),
                    })?
                    .to_string();
                let text = msg["content"].as_str().unwrap_or("").to_string();
                messages.push(CanonicalMessage {
                    role: Role::Tool,
                    content: vec![CanonicalContent::ToolResult {
                        tool_use_id: tool_call_id,
                        content: vec![CanonicalContent::Text { text }],
                        is_error: None,
                    }],
                    name: None,
                });
            }
            _ => {
                // user or unknown → user
                let content = decode_user_content(&msg["content"]);
                messages.push(CanonicalMessage {
                    role: Role::User,
                    content,
                    name: msg["name"].as_str().map(String::from),
                });
            }
        }
    }

    let tools: Vec<ToolDef> = body["tools"]
        .as_array()
        .unwrap_or(&vec![])
        .iter()
        .filter_map(|t| {
            let name = t["function"]["name"].as_str()?.to_string();
            let description = t["function"]["description"].as_str().map(String::from);
            let parameters = t["function"]["parameters"].clone();
            Some(ToolDef {
                name,
                description,
                parameters,
            })
        })
        .collect();

    let tool_choice = decode_tool_choice(&body["tool_choice"]);

    let response_format = decode_response_format(&body["response_format"]);

    let sampling = Sampling {
        temperature: body["temperature"].as_f64().map(|v| v as f32),
        top_p: body["top_p"].as_f64().map(|v| v as f32),
        max_tokens: body["max_tokens"].as_u64().map(|v| v as u32),
        stop: body["stop"].as_array().map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        }),
        seed: body["seed"].as_u64(),
        presence_penalty: body["presence_penalty"].as_f64().map(|v| v as f32),
        frequency_penalty: body["frequency_penalty"].as_f64().map(|v| v as f32),
        n: body["n"].as_u64().map(|v| v as u8),
        top_k: None,
        reasoning_effort: body["reasoning_effort"].as_str().map(String::from),
        service_tier: None,
    };

    Ok(CanonicalChatRequest {
        id: request_id.clone(),
        alias,
        messages,
        tools,
        tool_choice,
        response_format,
        sampling,
        meta: RequestMeta {
            user: body["user"].as_str().map(String::from),
            extra: Default::default(),
        },
        stream,
        loss_report: Default::default(),
    })
}

fn decode_user_content(val: &Value) -> Vec<CanonicalContent> {
    if let Some(text) = val.as_str() {
        return vec![CanonicalContent::Text {
            text: text.to_string(),
        }];
    }
    if let Some(arr) = val.as_array() {
        return arr
            .iter()
            .filter_map(|part| match part["type"].as_str() {
                Some("text") => Some(CanonicalContent::Text {
                    text: part["text"].as_str().unwrap_or("").to_string(),
                }),
                Some("image_url") => {
                    let url = part["image_url"]["url"].as_str().unwrap_or("").to_string();
                    let detail = part["image_url"]["detail"].as_str().map(String::from);
                    Some(CanonicalContent::Image {
                        url,
                        media_type: None,
                        detail,
                    })
                }
                _ => None,
            })
            .collect();
    }
    vec![]
}

fn decode_tool_choice(val: &Value) -> Option<ToolChoice> {
    if val.is_null() {
        return None;
    }
    if let Some(s) = val.as_str() {
        return Some(match s {
            "none" => ToolChoice::None,
            "required" => ToolChoice::Required,
            _ => ToolChoice::Auto,
        });
    }
    if val.is_object() {
        if let Some(name) = val["function"]["name"].as_str() {
            return Some(ToolChoice::Tool {
                name: name.to_string(),
            });
        }
    }
    None
}

fn decode_response_format(val: &Value) -> Option<ResponseFormat> {
    if val.is_null() {
        return None;
    }
    match val["type"].as_str() {
        Some("json_object") => Some(ResponseFormat::JsonObject),
        Some("json_schema") => {
            let schema = val["json_schema"]["schema"].clone();
            let strict = val["json_schema"]["strict"].as_bool();
            Some(ResponseFormat::JsonSchema { schema, strict })
        }
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
        decode_request(body, "gpt-4o".into(), false, "req-1".into(), "key-1".into()).unwrap()
    }

    #[test]
    fn basic_user_message() {
        let req = decode(json!({"messages": [{"role": "user", "content": "Hi"}]}));
        assert_eq!(req.messages.len(), 1);
        assert_eq!(req.messages[0].role, Role::User);
    }

    #[test]
    fn system_message_decoded() {
        let req = decode(json!({"messages": [
            {"role": "system", "content": "Be helpful"},
            {"role": "user", "content": "Hello"}
        ]}));
        assert_eq!(req.messages[0].role, Role::System);
    }

    #[test]
    fn assistant_tool_calls_decoded() {
        let body = json!({"messages": [{
            "role": "assistant",
            "content": null,
            "tool_calls": [{
                "id": "call_1",
                "type": "function",
                "function": {"name": "search", "arguments": "{\"q\":\"rust\"}"}
            }]
        }]});
        let req = decode(body);
        if let CanonicalContent::ToolUse { id, name, .. } = &req.messages[0].content[0] {
            assert_eq!(id, "call_1");
            assert_eq!(name, "search");
        } else {
            panic!("expected ToolUse");
        }
    }

    #[test]
    fn tool_result_decoded() {
        let body = json!({"messages": [{
            "role": "tool",
            "tool_call_id": "call_1",
            "content": "Paris"
        }]});
        let req = decode(body);
        if let CanonicalContent::ToolResult { tool_use_id, .. } = &req.messages[0].content[0] {
            assert_eq!(tool_use_id, "call_1");
        } else {
            panic!("expected ToolResult");
        }
    }

    #[test]
    fn missing_messages_returns_error() {
        let res = decode_request(json!({}), "gpt-4o".into(), false, "r".into(), "k".into());
        assert!(matches!(res, Err(CodecError::MissingField { .. })));
    }

    #[test]
    fn json_schema_response_format() {
        let body = json!({
            "messages": [{"role": "user", "content": "hi"}],
            "response_format": {
                "type": "json_schema",
                "json_schema": {"name": "r", "schema": {"type": "object"}, "strict": true}
            }
        });
        let req = decode(body);
        assert!(matches!(
            req.response_format,
            Some(ResponseFormat::JsonSchema { .. })
        ));
    }
}
