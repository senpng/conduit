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
            "system" | "developer" => {
                // CLIProxyAPI / OpenAI: developer is system-equivalent on chat.
                let content = decode_textish_content(&msg["content"]);
                messages.push(CanonicalMessage {
                    role: Role::System,
                    content,
                    name: msg["name"].as_str().map(String::from),
                });
            }
            "assistant" => {
                let mut content: Vec<CanonicalContent> = Vec::new();

                // Text part (string or multimodal array)
                content.extend(decode_textish_content(&msg["content"]));

                // reasoning_content → Thinking (CLIProxyAPI multi-turn parity)
                if let Some(rc) = msg["reasoning_content"].as_str() {
                    if !rc.is_empty() {
                        content.push(CanonicalContent::Thinking {
                            thinking: rc.to_string(),
                            // Stamp gpt# so encode_request maps it back to reasoning_content.
                            signature: Some("gpt#conduit".into()),
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
                let text = content_as_plain_text(&msg["content"]);
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

    // Prefer max_tokens; fall back to max_completion_tokens (newer OpenAI clients).
    let max_tokens = body["max_tokens"]
        .as_u64()
        .or_else(|| body["max_completion_tokens"].as_u64())
        .map(|v| v as u32);

    let sampling = Sampling {
        temperature: body["temperature"].as_f64().map(|v| v as f32),
        top_p: body["top_p"].as_f64().map(|v| v as f32),
        max_tokens,
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
        service_tier: body["service_tier"].as_str().map(String::from),
    };

    Ok(CanonicalChatRequest {
        id: request_id.clone(),
        alias,
        messages,
        tools,
        tool_choice,
        response_format,
        sampling,
        meta: {
            let mut meta = RequestMeta {
                user: body["user"].as_str().map(String::from),
                extra: Default::default(),
            };
            // Session affinity fields (gateway pool pins by session, not API key).
            for key in [
                "conversation_id",
                "session_id",
                "previous_response_id",
            ] {
                if let Some(s) = body[key].as_str().map(str::trim).filter(|s| !s.is_empty()) {
                    meta.extra
                        .insert(key.into(), serde_json::Value::String(s.to_string()));
                }
            }
            if let Some(m) = body.get("metadata").filter(|v| !v.is_null()) {
                meta.extra.insert("metadata".into(), m.clone());
            }
            // parallel_tool_calls lives in meta.extra for round-trip (CLIProxyAPI parity).
            if let Some(v) = body.get("parallel_tool_calls").filter(|v| v.is_boolean()) {
                meta.extra
                    .insert("parallel_tool_calls".into(), v.clone());
            }
            meta
        },
        stream,
        loss_report: Default::default(),
    })
}

/// Decode system/developer content: plain string or multimodal array of text parts.
fn decode_textish_content(val: &Value) -> Vec<CanonicalContent> {
    if let Some(text) = val.as_str() {
        if text.is_empty() {
            return vec![];
        }
        return vec![CanonicalContent::Text {
            text: text.to_string(),
        }];
    }
    if let Some(arr) = val.as_array() {
        return arr
            .iter()
            .filter_map(|part| {
                if let Some(s) = part.as_str() {
                    if s.is_empty() {
                        return None;
                    }
                    return Some(CanonicalContent::Text {
                        text: s.to_string(),
                    });
                }
                let ty = part.get("type").and_then(|t| t.as_str()).unwrap_or("text");
                if matches!(ty, "text" | "input_text" | "output_text") {
                    let text = part["text"].as_str().unwrap_or("").to_string();
                    if text.is_empty() {
                        None
                    } else {
                        Some(CanonicalContent::Text { text })
                    }
                } else {
                    None
                }
            })
            .collect();
    }
    vec![]
}

fn content_as_plain_text(val: &Value) -> String {
    if let Some(text) = val.as_str() {
        return text.to_string();
    }
    decode_textish_content(val)
        .into_iter()
        .filter_map(|c| match c {
            CanonicalContent::Text { text } => Some(text),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("")
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
                Some("text") | Some("input_text") => Some(CanonicalContent::Text {
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
    fn developer_role_maps_to_system() {
        let req = decode(json!({"messages": [
            {"role": "developer", "content": "You are a coding agent"},
            {"role": "user", "content": "Hello"}
        ]}));
        assert_eq!(req.messages[0].role, Role::System);
        assert!(
            matches!(&req.messages[0].content[0], CanonicalContent::Text { text } if text == "You are a coding agent")
        );
    }

    #[test]
    fn system_array_content_kept() {
        let req = decode(json!({"messages": [
            {"role": "system", "content": [
                {"type": "text", "text": "part-a"},
                {"type": "text", "text": "part-b"}
            ]},
            {"role": "user", "content": "hi"}
        ]}));
        assert_eq!(req.messages[0].role, Role::System);
        assert_eq!(req.messages[0].content.len(), 2);
        assert!(
            matches!(&req.messages[0].content[0], CanonicalContent::Text { text } if text == "part-a")
        );
        assert!(
            matches!(&req.messages[0].content[1], CanonicalContent::Text { text } if text == "part-b")
        );
    }

    #[test]
    fn assistant_reasoning_content_decoded() {
        let body = json!({"messages": [{
            "role": "assistant",
            "content": "answer",
            "reasoning_content": "I thought carefully"
        }]});
        let req = decode(body);
        let thinking = req.messages[0]
            .content
            .iter()
            .find_map(|c| match c {
                CanonicalContent::Thinking { thinking, .. } => Some(thinking.as_str()),
                _ => None,
            });
        assert_eq!(thinking, Some("I thought carefully"));
        assert!(
            req.messages[0].content.iter().any(
                |c| matches!(c, CanonicalContent::Text { text } if text == "answer")
            )
        );
    }

    #[test]
    fn max_completion_tokens_fallback() {
        let req = decode(json!({
            "messages": [{"role": "user", "content": "hi"}],
            "max_completion_tokens": 128
        }));
        assert_eq!(req.sampling.max_tokens, Some(128));
    }

    #[test]
    fn max_tokens_preferred_over_max_completion_tokens() {
        let req = decode(json!({
            "messages": [{"role": "user", "content": "hi"}],
            "max_tokens": 64,
            "max_completion_tokens": 128
        }));
        assert_eq!(req.sampling.max_tokens, Some(64));
    }

    #[test]
    fn parallel_tool_calls_and_service_tier() {
        let req = decode(json!({
            "messages": [{"role": "user", "content": "hi"}],
            "parallel_tool_calls": true,
            "service_tier": "priority"
        }));
        assert_eq!(req.sampling.service_tier.as_deref(), Some("priority"));
        assert_eq!(
            req.meta.extra.get("parallel_tool_calls"),
            Some(&json!(true))
        );
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
