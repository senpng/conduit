use conduit_ir::canonical::{
    CanonicalChatRequest, CanonicalContent, CanonicalMessage, ResponseFormat, Role, ToolChoice,
    ToolDef,
};
use serde_json::{json, Value};

/// Encode a canonical request into the OpenAI `/v1/chat/completions` wire format.
///
/// Aligns with CLIProxyAPI `ConvertClaudeRequestToOpenAI` for message shape:
/// - assistant text + tool_calls + reasoning_content in one message
/// - tool_result emitted as `role: tool` before remaining user content
/// - `thinking` → `reasoning_effort` when present on sampling
pub fn encode_request(req: &CanonicalChatRequest, stream: bool) -> Value {
    let mut messages: Vec<Value> = Vec::new();

    // System messages → single system turn.
    let system_text: String = req
        .messages
        .iter()
        .filter(|m| m.role == Role::System)
        .flat_map(|m| m.content.iter())
        .filter_map(|c| {
            if let CanonicalContent::Text { text } = c {
                Some(text.as_str())
            } else {
                None
            }
        })
        .collect::<Vec<_>>()
        .join("\n");

    if !system_text.is_empty() {
        messages.push(json!({"role": "system", "content": system_text}));
    }

    let non_system: Vec<&CanonicalMessage> = req
        .messages
        .iter()
        .filter(|m| m.role != Role::System)
        .collect();
    messages.extend(encode_messages(&non_system));

    let mut body = json!({
        "model": req.alias,
        "messages": messages,
        "stream": stream,
    });

    // Tools
    if !req.tools.is_empty() {
        body["tools"] = json!(encode_tools(&req.tools));
        if let Some(tc) = &req.tool_choice {
            body["tool_choice"] = encode_tool_choice(tc);
        }
    }

    // Sampling parameters
    let s = &req.sampling;
    if let Some(t) = s.temperature {
        body["temperature"] = json!(t);
    }
    if let Some(p) = s.top_p {
        body["top_p"] = json!(p);
    }
    if let Some(mt) = s.max_tokens {
        body["max_tokens"] = json!(mt);
    }
    if let Some(stop) = &s.stop {
        if !stop.is_empty() {
            body["stop"] = if stop.len() == 1 {
                json!(stop[0])
            } else {
                json!(stop)
            };
        }
    }
    if let Some(seed) = s.seed {
        body["seed"] = json!(seed);
    }
    if let Some(pp) = s.presence_penalty {
        body["presence_penalty"] = json!(pp);
    }
    if let Some(fp) = s.frequency_penalty {
        body["frequency_penalty"] = json!(fp);
    }
    if let Some(n) = s.n {
        body["n"] = json!(n);
    }
    if let Some(effort) = &s.reasoning_effort {
        if !effort.is_empty() {
            body["reasoning_effort"] = json!(effort);
        }
    }

    // Response format
    if let Some(rf) = &req.response_format {
        match rf {
            ResponseFormat::Text => {}
            ResponseFormat::JsonObject => {
                body["response_format"] = json!({"type": "json_object"});
            }
            ResponseFormat::JsonSchema { schema, strict } => {
                body["response_format"] = json!({
                    "type": "json_schema",
                    "json_schema": {
                        "name": "response",
                        "schema": schema,
                        "strict": strict.unwrap_or(false),
                    }
                });
            }
            _ => {}
        }
    }

    if stream {
        body["stream_options"] = json!({"include_usage": true});
    }

    body
}

fn encode_messages(messages: &[&CanonicalMessage]) -> Vec<Value> {
    let mut result = Vec::new();
    for msg in messages {
        match msg.role {
            Role::Assistant => {
                if let Some(v) = encode_assistant_message(msg) {
                    result.push(v);
                }
            }
            Role::User | Role::Tool => {
                result.extend(encode_user_or_tool_message(msg));
            }
            Role::System => {
                // Already hoisted.
            }
            _ => {
                result.extend(encode_user_or_tool_message(msg));
            }
        }
    }
    result
}

/// CLIProxyAPI: one assistant message with content + tool_calls + reasoning_content.
fn encode_assistant_message(msg: &CanonicalMessage) -> Option<Value> {
    let mut tool_calls: Vec<Value> = Vec::new();
    let mut text_parts: Vec<Value> = Vec::new();
    let mut reasoning_parts: Vec<String> = Vec::new();

    for c in &msg.content {
        match c {
            CanonicalContent::ToolUse { id, name, input } => {
                let args = if input.is_string() {
                    input.as_str().unwrap_or("{}").to_string()
                } else {
                    input.to_string()
                };
                tool_calls.push(json!({
                    "id": id,
                    "type": "function",
                    "function": {
                        "name": name,
                        "arguments": args,
                    }
                }));
            }
            CanonicalContent::Text { text } => {
                if !text.is_empty() {
                    text_parts.push(json!({"type": "text", "text": text}));
                }
            }
            CanonicalContent::Image {
                url,
                media_type: _,
                detail,
            } => {
                let det = detail.as_deref().unwrap_or("auto");
                text_parts.push(json!({
                    "type": "image_url",
                    "image_url": {"url": url, "detail": det},
                }));
            }
            CanonicalContent::Thinking {
                thinking,
                signature,
            } => {
                if should_map_thinking_to_reasoning(thinking, signature) {
                    reasoning_parts.push(thinking.clone());
                }
            }
            CanonicalContent::ToolResult { .. } => {
                // Tool results on assistant are unexpected; ignore.
            }
            _ => {}
        }
    }

    let has_content = !text_parts.is_empty();
    let has_tools = !tool_calls.is_empty();
    let has_reasoning = !reasoning_parts.is_empty();
    if !has_content && !has_tools && !has_reasoning {
        return None;
    }

    let mut msg_json = json!({"role": "assistant"});
    if has_content {
        if text_parts.len() == 1 {
            if let Some(t) = text_parts[0]["text"].as_str() {
                msg_json["content"] = json!(t);
            } else {
                msg_json["content"] = json!(text_parts);
            }
        } else {
            msg_json["content"] = json!(text_parts);
        }
    } else {
        // OpenAI requires content field when only tools/reasoning.
        msg_json["content"] = if has_tools { Value::Null } else { json!("") };
    }
    if has_reasoning {
        msg_json["reasoning_content"] = json!(reasoning_parts.join("\n\n"));
    }
    if has_tools {
        msg_json["tool_calls"] = json!(tool_calls);
    }
    Some(msg_json)
}

/// tool_result blocks → `role: tool` first, then remaining user content.
fn encode_user_or_tool_message(msg: &CanonicalMessage) -> Vec<Value> {
    let mut out = Vec::new();
    let mut other: Vec<&CanonicalContent> = Vec::new();

    for c in &msg.content {
        match c {
            CanonicalContent::ToolResult {
                tool_use_id,
                content,
                is_error: _,
            } => {
                let text: String = content
                    .iter()
                    .filter_map(|c| {
                        if let CanonicalContent::Text { text } = c {
                            Some(text.as_str())
                        } else {
                            None
                        }
                    })
                    .collect::<Vec<_>>()
                    .join("");
                out.push(json!({
                    "role": "tool",
                    "tool_call_id": tool_use_id,
                    "content": text,
                }));
            }
            other_c => other.push(other_c),
        }
    }

    if !other.is_empty() {
        let content = encode_content_parts(&other.iter().map(|c| (*c).clone()).collect::<Vec<_>>());
        // Skip empty content after stripping tool results.
        let empty = match &content {
            Value::String(s) => s.is_empty(),
            Value::Array(a) => a.is_empty(),
            _ => false,
        };
        if !empty {
            out.push(json!({
                "role": "user",
                "content": content,
            }));
        }
    }
    out
}

fn should_map_thinking_to_reasoning(thinking: &str, signature: &Option<String>) -> bool {
    if thinking.trim().is_empty() {
        return false;
    }
    // CLIProxyAPI: only GPT-compatible signatures. We stamp `gpt#conduit` on
    // outbound thinking blocks; also accept any `gpt#` prefix.
    match signature {
        Some(sig) => {
            let s = sig.trim();
            !s.is_empty() && (s.starts_with("gpt#") || s.starts_with("gpt"))
        }
        None => false,
    }
}

fn encode_content_parts(content: &[CanonicalContent]) -> Value {
    if content.len() == 1 {
        if let CanonicalContent::Text { text } = &content[0] {
            return json!(text);
        }
    }

    let parts: Vec<Value> = content
        .iter()
        .filter_map(|c| match c {
            CanonicalContent::Text { text } => Some(json!({"type": "text", "text": text})),
            CanonicalContent::Image {
                url,
                media_type: _,
                detail,
            } => {
                let det = detail.as_deref().unwrap_or("auto");
                Some(json!({
                    "type": "image_url",
                    "image_url": {"url": url, "detail": det},
                }))
            }
            CanonicalContent::Thinking { .. } => None,
            _ => None,
        })
        .collect();

    if parts.is_empty() {
        json!("")
    } else {
        json!(parts)
    }
}

fn encode_tools(tools: &[ToolDef]) -> Vec<Value> {
    tools
        .iter()
        .map(|t| {
            let params = normalize_object_schema(t.parameters.clone());
            let mut func = json!({
                "name": t.name,
                "parameters": params,
            });
            if let Some(desc) = &t.description {
                func["description"] = json!(desc);
            }
            json!({"type": "function", "function": func})
        })
        .collect()
}

/// Ensure object schemas have a `properties` map (CLIProxyAPI normalizeObjectSchemaProperties).
fn normalize_object_schema(mut schema: Value) -> Value {
    match &mut schema {
        Value::Object(map) => {
            if map.get("type").and_then(|t| t.as_str()) == Some("object")
                && !map.contains_key("properties")
            {
                map.insert("properties".into(), json!({}));
            }
            // Recurse into nested values
            let keys: Vec<String> = map.keys().cloned().collect();
            for k in keys {
                if let Some(v) = map.get_mut(&k) {
                    *v = normalize_object_schema(v.clone());
                }
            }
        }
        Value::Array(arr) => {
            for item in arr.iter_mut() {
                *item = normalize_object_schema(item.clone());
            }
        }
        _ => {}
    }
    schema
}

/// Map CanonicalToolChoice → OpenAI tool_choice value.
pub fn encode_tool_choice(tc: &ToolChoice) -> Value {
    match tc {
        ToolChoice::Auto => json!("auto"),
        ToolChoice::None => json!("none"),
        ToolChoice::Required => json!("required"),
        ToolChoice::AnyOf { .. } => json!("required"),
        ToolChoice::Tool { name } => {
            json!({"type": "function", "function": {"name": name}})
        }
        _ => json!("auto"),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use conduit_ir::canonical::{CanonicalChatRequest, CanonicalMessage, Sampling, ToolDef};

    use super::*;

    fn make_request() -> CanonicalChatRequest {
        CanonicalChatRequest::new("gpt-4o", vec![CanonicalMessage::user("Hello")])
    }

    #[test]
    fn basic_encode() {
        let req = make_request();
        let v = encode_request(&req, false);
        assert_eq!(v["model"].as_str().unwrap(), "gpt-4o");
        let msgs = v["messages"].as_array().unwrap();
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0]["role"].as_str().unwrap(), "user");
        assert_eq!(msgs[0]["content"].as_str().unwrap(), "Hello");
    }

    #[test]
    fn system_message_hoisted() {
        let req = CanonicalChatRequest::new(
            "gpt-4o",
            vec![
                CanonicalMessage::system("Be helpful"),
                CanonicalMessage::user("Hi"),
            ],
        );
        let v = encode_request(&req, false);
        let msgs = v["messages"].as_array().unwrap();
        assert_eq!(msgs[0]["role"].as_str().unwrap(), "system");
        assert_eq!(msgs[0]["content"].as_str().unwrap(), "Be helpful");
        assert_eq!(msgs[1]["role"].as_str().unwrap(), "user");
    }

    #[test]
    fn stream_options_included_when_streaming() {
        let req = make_request();
        let v = encode_request(&req, true);
        assert_eq!(v["stream"].as_bool(), Some(true));
        assert!(v["stream_options"]["include_usage"].as_bool().unwrap());
    }

    #[test]
    fn sampling_params_forwarded() {
        let mut req = make_request();
        req.sampling = Sampling {
            temperature: Some(0.7),
            top_p: Some(0.9),
            max_tokens: Some(256),
            seed: Some(42),
            ..Default::default()
        };
        let v = encode_request(&req, false);
        assert!((v["temperature"].as_f64().unwrap() - 0.7).abs() < 1e-6);
        assert_eq!(v["max_tokens"].as_u64().unwrap(), 256);
        assert_eq!(v["seed"].as_u64().unwrap(), 42);
    }

    #[test]
    fn reasoning_effort_forwarded() {
        let mut req = make_request();
        req.sampling.reasoning_effort = Some("high".into());
        let v = encode_request(&req, false);
        assert_eq!(v["reasoning_effort"].as_str().unwrap(), "high");
    }

    #[test]
    fn assistant_keeps_text_with_tool_calls() {
        let msg = CanonicalMessage {
            role: Role::Assistant,
            content: vec![
                CanonicalContent::Text {
                    text: "Calling tools".into(),
                },
                CanonicalContent::ToolUse {
                    id: "c1".into(),
                    name: "search".into(),
                    input: json!({"q": "x"}),
                },
            ],
            name: None,
        };
        let req = CanonicalChatRequest::new("gpt-4o", vec![msg]);
        let v = encode_request(&req, false);
        let a = &v["messages"][0];
        assert_eq!(a["content"].as_str().unwrap(), "Calling tools");
        assert_eq!(a["tool_calls"][0]["id"].as_str().unwrap(), "c1");
    }

    #[test]
    fn tool_result_then_user_text() {
        let msg = CanonicalMessage {
            role: Role::User,
            content: vec![
                CanonicalContent::ToolResult {
                    tool_use_id: "c1".into(),
                    content: vec![CanonicalContent::Text {
                        text: "result".into(),
                    }],
                    is_error: None,
                },
                CanonicalContent::Text {
                    text: "thanks".into(),
                },
            ],
            name: None,
        };
        let req = CanonicalChatRequest::new("gpt-4o", vec![msg]);
        let v = encode_request(&req, false);
        let msgs = v["messages"].as_array().unwrap();
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0]["role"].as_str().unwrap(), "tool");
        assert_eq!(msgs[0]["content"].as_str().unwrap(), "result");
        assert_eq!(msgs[1]["role"].as_str().unwrap(), "user");
        assert_eq!(msgs[1]["content"].as_str().unwrap(), "thanks");
    }

    #[test]
    fn gpt_thinking_maps_to_reasoning_content() {
        let msg = CanonicalMessage {
            role: Role::Assistant,
            content: vec![CanonicalContent::Thinking {
                thinking: "step1".into(),
                signature: Some("gpt#conduit".into()),
            }],
            name: None,
        };
        let req = CanonicalChatRequest::new("gpt-4o", vec![msg]);
        let v = encode_request(&req, false);
        assert_eq!(
            v["messages"][0]["reasoning_content"].as_str().unwrap(),
            "step1"
        );
    }

    #[test]
    fn tool_choice_anyof_degrades_to_required() {
        let mut req = make_request();
        req.tools = vec![ToolDef {
            name: "search".into(),
            description: None,
            parameters: serde_json::json!({"type": "object"}),
        }];
        req.tool_choice = Some(ToolChoice::AnyOf {
            names: vec!["search".into()],
        });
        let v = encode_request(&req, false);
        assert_eq!(v["tool_choice"].as_str().unwrap(), "required");
    }

    #[test]
    fn response_format_json_object() {
        let mut req = make_request();
        req.response_format = Some(ResponseFormat::JsonObject);
        let v = encode_request(&req, false);
        assert_eq!(
            v["response_format"]["type"].as_str().unwrap(),
            "json_object"
        );
    }

    #[test]
    fn object_schema_gets_properties() {
        let mut req = make_request();
        req.tools = vec![ToolDef {
            name: "f".into(),
            description: None,
            parameters: json!({"type": "object"}),
        }];
        let v = encode_request(&req, false);
        assert!(v["tools"][0]["function"]["parameters"]["properties"].is_object());
    }
}
