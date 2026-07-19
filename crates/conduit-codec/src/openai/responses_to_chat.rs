//! Convert OpenAI Responses-shaped bodies into Chat Completions shape.
//!
//! Aligns with CLIProxyAPI `shouldTreatAsResponsesFormat` +
//! `ConvertOpenAIResponsesRequestToOpenAIChatCompletions` for the common
//! coding-client path (instructions, input string/array, tools, function
//! calls/outputs, reasoning.effort, parallel_tool_calls).

use serde_json::{json, Value};

/// True when a body looks like Responses API payload sent to chat/completions.
///
/// CLIProxyAPI: no `messages`, but has `input` and/or `instructions`.
pub fn should_treat_as_responses_format(body: &Value) -> bool {
    if body.get("messages").is_some() {
        return false;
    }
    body.get("input").is_some() || body.get("instructions").is_some()
}

/// Convert a Responses-shaped request into Chat Completions wire JSON.
///
/// Pure function: input is never mutated; returns a new object with `messages`.
pub fn convert_responses_to_chat_completions(body: &Value, stream: bool) -> Value {
    let model = body
        .get("model")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    let mut messages: Vec<Value> = Vec::new();

    // instructions → system message
    if let Some(instructions) = body.get("instructions").and_then(|v| v.as_str()) {
        if !instructions.is_empty() {
            messages.push(json!({
                "role": "system",
                "content": instructions,
            }));
        }
    }

    // input → messages
    match body.get("input") {
        Some(Value::String(text)) => {
            messages.push(json!({
                "role": "user",
                "content": text,
            }));
        }
        Some(Value::Array(items)) => {
            convert_input_items(items, &mut messages);
        }
        _ => {}
    }

    let mut out = json!({
        "model": model,
        "messages": messages,
        "stream": stream,
    });

    // max_output_tokens → max_tokens
    if let Some(n) = body.get("max_output_tokens").and_then(|v| v.as_u64()) {
        out["max_tokens"] = json!(n);
    }

    if let Some(v) = body.get("parallel_tool_calls").filter(|v| v.is_boolean()) {
        out["parallel_tool_calls"] = v.clone();
    }

    if let Some(temp) = body.get("temperature") {
        out["temperature"] = temp.clone();
    }
    if let Some(top_p) = body.get("top_p") {
        out["top_p"] = top_p.clone();
    }
    if let Some(user) = body.get("user") {
        out["user"] = user.clone();
    }
    if let Some(tier) = body.get("service_tier") {
        out["service_tier"] = tier.clone();
    }

    // reasoning.effort → reasoning_effort
    if let Some(effort) = body
        .pointer("/reasoning/effort")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        out["reasoning_effort"] = json!(effort);
    }

    // tools (Responses function shape → chat function wrapper)
    let mut chat_tools: Vec<Value> = Vec::new();
    if let Some(tools) = body.get("tools").and_then(|v| v.as_array()) {
        for tool in tools {
            if let Some(converted) = convert_responses_tool(tool) {
                chat_tools.push(converted);
            }
        }
    }
    // additional_tools input items (Codex Desktop)
    if let Some(items) = body.get("input").and_then(|v| v.as_array()) {
        for item in items {
            if item.get("type").and_then(|t| t.as_str()) == Some("additional_tools") {
                if let Some(tools) = item.get("tools").and_then(|v| v.as_array()) {
                    for tool in tools {
                        if let Some(converted) = convert_responses_tool(tool) {
                            chat_tools.push(converted);
                        }
                    }
                }
            }
        }
    }
    if !chat_tools.is_empty() {
        out["tools"] = json!(chat_tools);
    }

    if let Some(tc) = body.get("tool_choice") {
        out["tool_choice"] = convert_tool_choice(tc);
    }

    // Preserve session affinity keys
    for key in ["conversation_id", "session_id", "previous_response_id", "metadata"] {
        if let Some(v) = body.get(key).filter(|v| !v.is_null()) {
            out[key] = v.clone();
        }
    }

    out
}

fn convert_input_items(items: &[Value], messages: &mut Vec<Value>) {
    let mut pending_tool_calls: Vec<Value> = Vec::new();
    let mut pending_reasoning = String::new();

    let flush_tool_calls = |pending: &mut Vec<Value>,
                            reasoning: &mut String,
                            messages: &mut Vec<Value>| {
        if pending.is_empty() {
            return;
        }
        let mut msg = json!({
            "role": "assistant",
            "tool_calls": pending.clone(),
        });
        if !reasoning.is_empty() {
            msg["reasoning_content"] = json!(std::mem::take(reasoning));
        }
        messages.push(msg);
        pending.clear();
    };

    let flush_reasoning = |reasoning: &mut String, messages: &mut Vec<Value>| {
        if reasoning.is_empty() {
            return;
        }
        messages.push(json!({
            "role": "assistant",
            "content": "",
            "reasoning_content": std::mem::take(reasoning),
        }));
    };

    for item in items {
        let item_type = item
            .get("type")
            .and_then(|t| t.as_str())
            .or_else(|| {
                if item.get("role").and_then(|r| r.as_str()).is_some() {
                    Some("message")
                } else {
                    None
                }
            })
            .unwrap_or("");

        if item_type != "function_call" && item_type != "custom_tool_call" {
            flush_tool_calls(&mut pending_tool_calls, &mut pending_reasoning, messages);
        }

        match item_type {
            "message" | "" => {
                let mut role = item
                    .get("role")
                    .and_then(|r| r.as_str())
                    .unwrap_or("user")
                    .to_string();
                // CLIProxyAPI maps developer → user on this conversion path.
                if role == "developer" {
                    role = "user".into();
                }
                if role != "assistant" {
                    flush_reasoning(&mut pending_reasoning, messages);
                }

                let content = convert_message_content(item.get("content"));
                let mut msg = json!({
                    "role": role,
                    "content": content,
                });
                if role == "assistant" {
                    let rc = item
                        .get("reasoning_content")
                        .and_then(|v| v.as_str())
                        .filter(|s| !s.is_empty())
                        .map(str::to_string)
                        .or_else(|| {
                            if pending_reasoning.is_empty() {
                                None
                            } else {
                                Some(std::mem::take(&mut pending_reasoning))
                            }
                        });
                    if let Some(rc) = rc {
                        msg["reasoning_content"] = json!(rc);
                    }
                }
                messages.push(msg);
            }
            "reasoning" => {
                let text = collect_reasoning_content(item);
                if pending_reasoning.is_empty() {
                    pending_reasoning = text;
                } else {
                    pending_reasoning.push_str(&text);
                }
            }
            "function_call" => {
                let call_id = item
                    .get("call_id")
                    .or_else(|| item.get("id"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let name = item.get("name").and_then(|v| v.as_str()).unwrap_or("");
                let arguments = item
                    .get("arguments")
                    .and_then(|v| v.as_str())
                    .unwrap_or("{}");
                pending_tool_calls.push(json!({
                    "id": call_id,
                    "type": "function",
                    "function": {
                        "name": name,
                        "arguments": arguments,
                    }
                }));
            }
            "function_call_output" | "custom_tool_call_output" => {
                let call_id = item
                    .get("call_id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let output = tool_output_text(item.get("output"));
                messages.push(json!({
                    "role": "tool",
                    "tool_call_id": call_id,
                    "content": output,
                }));
            }
            "custom_tool_call" => {
                let call_id = item
                    .get("call_id")
                    .or_else(|| item.get("id"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let name = item.get("name").and_then(|v| v.as_str()).unwrap_or("");
                let input = item.get("input").and_then(|v| v.as_str()).unwrap_or("");
                let wrapped = json!({"input": input}).to_string();
                pending_tool_calls.push(json!({
                    "id": call_id,
                    "type": "function",
                    "function": {
                        "name": name,
                        "arguments": wrapped,
                    }
                }));
            }
            "additional_tools" => {
                // Handled at top-level tools merge.
            }
            _ => {}
        }
    }

    flush_tool_calls(&mut pending_tool_calls, &mut pending_reasoning, messages);
    flush_reasoning(&mut pending_reasoning, messages);
}

fn convert_message_content(content: Option<&Value>) -> Value {
    match content {
        Some(Value::String(s)) => json!(s),
        Some(Value::Array(parts)) => {
            let mut out = Vec::new();
            for part in parts {
                let ty = part
                    .get("type")
                    .and_then(|t| t.as_str())
                    .unwrap_or("input_text");
                match ty {
                    "input_text" | "output_text" | "text" => {
                        let text = part.get("text").and_then(|t| t.as_str()).unwrap_or("");
                        out.push(json!({"type": "text", "text": text}));
                    }
                    "input_image" => {
                        let url = part
                            .get("image_url")
                            .and_then(|u| u.as_str())
                            .unwrap_or("");
                        let mut img = json!({
                            "type": "image_url",
                            "image_url": {"url": url},
                        });
                        if let Some(detail) = part.get("detail") {
                            img["image_url"]["detail"] = detail.clone();
                        }
                        out.push(img);
                    }
                    _ => {}
                }
            }
            if out.len() == 1 {
                if let Some(t) = out[0]["text"].as_str() {
                    return json!(t);
                }
            }
            json!(out)
        }
        _ => json!(""),
    }
}

fn collect_reasoning_content(item: &Value) -> String {
    let mut parts = Vec::new();
    if let Some(summary) = item.get("summary").and_then(|v| v.as_array()) {
        for s in summary {
            if s.get("type").and_then(|t| t.as_str()) == Some("summary_text") {
                if let Some(text) = s.get("text").and_then(|t| t.as_str()) {
                    parts.push(text);
                }
            }
        }
    }
    if parts.is_empty() {
        "[reasoning unavailable]".into()
    } else {
        parts.join("")
    }
}

fn tool_output_text(output: Option<&Value>) -> String {
    match output {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(parts)) => parts
            .iter()
            .filter_map(|p| p.get("text").and_then(|t| t.as_str()))
            .collect::<Vec<_>>()
            .join(""),
        Some(other) => other.to_string(),
        None => String::new(),
    }
}

fn convert_responses_tool(tool: &Value) -> Option<Value> {
    let ty = tool.get("type").and_then(|t| t.as_str()).unwrap_or("function");
    if ty != "function" {
        return None;
    }
    // Responses tools may be flat {type,name,description,parameters} or nested function.
    if tool.get("function").is_some() {
        return Some(tool.clone());
    }
    let name = tool.get("name").and_then(|v| v.as_str())?;
    let mut func = json!({
        "name": name,
        "parameters": tool.get("parameters").cloned().unwrap_or(json!({})),
    });
    if let Some(desc) = tool.get("description") {
        func["description"] = desc.clone();
    }
    Some(json!({
        "type": "function",
        "function": func,
    }))
}

fn convert_tool_choice(tc: &Value) -> Value {
    // Responses may use {"type":"function","name":"..."} without nested function.
    if let Some(obj) = tc.as_object() {
        if obj.get("type").and_then(|t| t.as_str()) == Some("function") {
            if let Some(name) = obj.get("name").and_then(|n| n.as_str()) {
                if obj.get("function").is_none() {
                    return json!({"type": "function", "function": {"name": name}});
                }
            }
        }
    }
    tc.clone()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn detect_responses_shape() {
        assert!(should_treat_as_responses_format(&json!({
            "model": "gpt-4o",
            "input": "hello"
        })));
        assert!(should_treat_as_responses_format(&json!({
            "model": "gpt-4o",
            "instructions": "be nice",
            "input": []
        })));
        assert!(!should_treat_as_responses_format(&json!({
            "model": "gpt-4o",
            "messages": [{"role": "user", "content": "hi"}]
        })));
        assert!(!should_treat_as_responses_format(&json!({
            "model": "gpt-4o",
            "messages": [],
            "input": "ignored because messages present"
        })));
    }

    #[test]
    fn convert_input_string_and_instructions() {
        let body = json!({
            "model": "gpt-4o",
            "instructions": "Be helpful",
            "input": "Hello world",
            "stream": false
        });
        let out = convert_responses_to_chat_completions(&body, false);
        assert!(out.get("messages").is_some());
        let msgs = out["messages"].as_array().unwrap();
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0]["role"], "system");
        assert_eq!(msgs[0]["content"], "Be helpful");
        assert_eq!(msgs[1]["role"], "user");
        assert_eq!(msgs[1]["content"], "Hello world");
        assert_eq!(out["model"], "gpt-4o");
        assert_eq!(out["stream"], false);
    }

    #[test]
    fn convert_input_array_with_tools() {
        let body = json!({
            "model": "gpt-4o",
            "input": [
                {
                    "type": "message",
                    "role": "user",
                    "content": [{"type": "input_text", "text": "search rust"}]
                }
            ],
            "tools": [{
                "type": "function",
                "name": "search",
                "description": "web search",
                "parameters": {"type": "object", "properties": {}}
            }],
            "max_output_tokens": 256,
            "parallel_tool_calls": true,
            "reasoning": {"effort": "high"}
        });
        let out = convert_responses_to_chat_completions(&body, true);
        assert_eq!(out["stream"], true);
        assert_eq!(out["max_tokens"], 256);
        assert_eq!(out["parallel_tool_calls"], true);
        assert_eq!(out["reasoning_effort"], "high");
        assert_eq!(out["tools"][0]["function"]["name"], "search");
        let msgs = out["messages"].as_array().unwrap();
        assert_eq!(msgs[0]["role"], "user");
        // single text part collapses to string
        assert_eq!(msgs[0]["content"], "search rust");
    }

    #[test]
    fn convert_function_call_and_output() {
        let body = json!({
            "model": "gpt-4o",
            "input": [
                {"type": "message", "role": "user", "content": "hi"},
                {
                    "type": "function_call",
                    "call_id": "call_1",
                    "name": "search",
                    "arguments": "{\"q\":\"x\"}"
                },
                {
                    "type": "function_call_output",
                    "call_id": "call_1",
                    "output": "result"
                }
            ]
        });
        let out = convert_responses_to_chat_completions(&body, false);
        let msgs = out["messages"].as_array().unwrap();
        assert_eq!(msgs[0]["role"], "user");
        assert_eq!(msgs[1]["role"], "assistant");
        assert_eq!(msgs[1]["tool_calls"][0]["id"], "call_1");
        assert_eq!(msgs[1]["tool_calls"][0]["function"]["name"], "search");
        assert_eq!(msgs[2]["role"], "tool");
        assert_eq!(msgs[2]["tool_call_id"], "call_1");
        assert_eq!(msgs[2]["content"], "result");
    }

    #[test]
    fn converted_body_decodes_via_chat_codec() {
        use crate::openai::OpenAiCodec;
        use crate::WireCodec;

        let body = json!({
            "model": "gpt-4o",
            "instructions": "sys",
            "input": "hello from responses"
        });
        assert!(should_treat_as_responses_format(&body));
        let chat = convert_responses_to_chat_completions(&body, false);
        assert!(chat.get("messages").is_some());
        let req = OpenAiCodec::decode_request(
            chat,
            "gpt-4o".into(),
            false,
            "req-1".into(),
            "key".into(),
        )
        .expect("decode after conversion must succeed");
        assert!(req
            .messages
            .iter()
            .any(|m| m.role == conduit_ir::canonical::Role::User));
    }
}
