//! [`OpenAIResponsesCodec`] wire encode/decode (non-stream helpers + `WireCodec`).

use std::collections::HashSet;

use conduit_ir::{
    canonical::{
        BlockDelta, BlockKind, CanonicalChatRequest, CanonicalChatResponse, CanonicalChunk,
        CanonicalContent, CanonicalMessage, FinishReason, Role, ToolChoice, ToolDef,
    },
    error::CodecError,
    loss::LossReport,
};
use serde_json::{json, Value};

use crate::WireCodec;

use super::helpers::{
    content_to_text, parse_usage, text_item_json, tool_item_json, usage_json,
};
use super::stream_decode::{decode_responses_sse_event_stateful, ResponsesStreamState};

pub struct OpenAIResponsesCodec;

/// Official Responses request fields preserved via `meta.extra` for re-emit.
/// Codex ChatGPT-account apply may still strip a subset of these.
const RESPONSES_PASSTHROUGH_KEYS: &[&str] = &[
    "background",
    "conversation",
    "max_tool_calls",
    "include",
    "truncation",
    "context_management",
    "prompt_cache_key",
    "prompt_cache_options",
    "prompt_cache_retention",
    "safety_identifier",
    "service_tier",
    "parallel_tool_calls",
    "text",
    "store",
    "stream_options",
];


impl WireCodec for OpenAIResponsesCodec {
    fn encode_request(req: &CanonicalChatRequest, stream: bool) -> (Value, LossReport) {
        let mut loss = LossReport::default();
        let mut input: Vec<Value> = Vec::new();

        for msg in &req.messages {
            match msg.role {
                Role::System => {
                    let text = content_to_text(&msg.content);
                    if !text.is_empty() {
                        // Plain string content is accepted; Codex maps system→developer later.
                        input.push(json!({
                            "type": "message",
                            "role": "system",
                            "content": [{
                                "type": "input_text",
                                "text": text,
                            }],
                        }));
                    }
                }
                Role::User | Role::Tool => {
                    // Claude/Anthropic puts tool_result blocks on user turns.
                    // Emit function_call_output items first (CLIProxyAPI order).
                    let mut text_parts: Vec<Value> = Vec::new();
                    for c in &msg.content {
                        match c {
                            CanonicalContent::ToolResult {
                                tool_use_id,
                                content,
                                ..
                            } => {
                                input.push(json!({
                                    "type": "function_call_output",
                                    "call_id": tool_use_id,
                                    "output": content_to_text(content),
                                }));
                            }
                            CanonicalContent::Text { text } if !text.is_empty() => {
                                text_parts.push(json!({
                                    "type": "input_text",
                                    "text": text,
                                }));
                            }
                            CanonicalContent::Image { url, .. } => {
                                text_parts.push(json!({
                                    "type": "input_image",
                                    "image_url": url,
                                }));
                            }
                            _ => {}
                        }
                    }
                    if !text_parts.is_empty() {
                        input.push(json!({
                            "type": "message",
                            "role": "user",
                            "content": text_parts,
                        }));
                    }
                }
                Role::Assistant => {
                    let text = content_to_text(&msg.content);
                    if !text.is_empty() {
                        input.push(json!({
                            "type": "message",
                            "role": "assistant",
                            "content": [{
                                "type": "output_text",
                                "text": text,
                            }],
                        }));
                    }
                    for c in &msg.content {
                        if let CanonicalContent::ToolUse {
                            id,
                            name,
                            input: args,
                        } = c
                        {
                            let args_str = if args.is_string() {
                                args.as_str().unwrap_or("{}").to_string()
                            } else {
                                args.to_string()
                            };
                            input.push(json!({
                                "type": "function_call",
                                "call_id": id,
                                "name": name,
                                "arguments": args_str,
                            }));
                        }
                    }
                }
                _ => {
                    loss.add(
                        "messages.role",
                        format!("{:?}", msg.role),
                        "omitted",
                        "unsupported role for Responses API",
                    );
                }
            }
        }

        let mut body = json!({
            "model": req.alias,
            "input": input,
            "stream": stream,
            // Required by ChatGPT-account Codex; API-key Responses may ignore it.
            "store": false,
        });

        if !req.tools.is_empty() {
            let tools: Vec<Value> = req
                .tools
                .iter()
                .map(|t| {
                    json!({
                        "type": "function",
                        "name": t.name,
                        "description": t.description,
                        "parameters": t.parameters,
                    })
                })
                .collect();
            body["tools"] = json!(tools);
            if let Some(tc) = &req.tool_choice {
                body["tool_choice"] = match tc {
                    ToolChoice::Auto => json!("auto"),
                    ToolChoice::Required => json!("required"),
                    ToolChoice::None => json!("none"),
                    ToolChoice::Tool { name } => json!({"type": "function", "name": name}),
                    ToolChoice::AnyOf { names } => {
                        loss.add(
                            "tool_choice",
                            format!("AnyOf({names:?})"),
                            "required",
                            "Responses AnyOf degraded to required",
                        );
                        json!("required")
                    }
                    _ => json!("auto"),
                };
            }
        }

        // Map Claude thinking / reasoning_effort → Responses reasoning config.
        if let Some(effort) = &req.sampling.reasoning_effort {
            if !effort.is_empty() {
                body["reasoning"] = json!({
                    "effort": effort,
                    "summary": "auto",
                });
            }
        }

        // Grok / API-key Responses may accept these; ChatGPT Codex strips them
        // via [`apply_codex_chatgpt_account_body`].
        if let Some(t) = req.sampling.temperature {
            body["temperature"] = json!(t);
        }
        if let Some(mt) = req.sampling.max_tokens {
            body["max_output_tokens"] = json!(mt);
        }
        if let Some(user) = &req.meta.user {
            if !user.is_empty() {
                body["user"] = json!(user);
            }
        }
        // P3: first-class Responses fields preserved via meta.extra.
        // Codex account apply may still strip forbidden keys afterward.
        for key in RESPONSES_PASSTHROUGH_KEYS {
            if let Some(v) = req.meta.extra.get(*key) {
                if !v.is_null() {
                    // Default encode sets store=false; only override when client set it.
                    if *key == "store" {
                        body["store"] = v.clone();
                        continue;
                    }
                    body[*key] = v.clone();
                }
            }
        }
        // previous_response_id / session keys also live in extra.
        if let Some(v) = req.meta.extra.get("previous_response_id") {
            if !v.is_null() {
                body["previous_response_id"] = v.clone();
            }
        }
        if let Some(v) = req.meta.extra.get("metadata") {
            if !v.is_null() {
                body["metadata"] = v.clone();
            }
        }

        (body, loss)
    }

    fn decode_request(
        body: Value,
        alias: String,
        stream: bool,
        request_id: String,
        _key_id: String,
    ) -> Result<CanonicalChatRequest, CodecError> {
        let input = body.get("input").ok_or_else(|| CodecError::MissingField {
            field: "input".into(),
        })?;
        let mut messages = Vec::new();

        if let Some(text) = input.as_str() {
            messages.push(CanonicalMessage::user(text));
        } else if let Some(items) = input.as_array() {
            validate_stateless_tool_outputs(items)?;
            for item in items {
                decode_input_item(item, &mut messages)?;
            }
        } else {
            return Err(CodecError::InvalidValue {
                field: "input".into(),
                value: "expected a string or array".into(),
            });
        }

        if let Some(instructions) = body.get("instructions").and_then(|v| v.as_str()) {
            if !instructions.is_empty() {
                messages.insert(0, CanonicalMessage::system(instructions));
            }
        }

        let tools = body
            .get("tools")
            .and_then(|v| v.as_array())
            .map(|tools| {
                tools
                    .iter()
                    .filter_map(|tool| {
                        if tool.get("type").and_then(|v| v.as_str()) != Some("function") {
                            return None;
                        }
                        tool.get("name")
                            .and_then(|v| v.as_str())
                            .map(|name| ToolDef {
                                name: name.to_string(),
                                description: tool
                                    .get("description")
                                    .and_then(|v| v.as_str())
                                    .map(str::to_string),
                                parameters: tool
                                    .get("parameters")
                                    .cloned()
                                    .unwrap_or_else(|| json!({})),
                            })
                    })
                    .collect()
            })
            .unwrap_or_default();

        Ok(CanonicalChatRequest {
            id: request_id,
            alias,
            messages,
            tools,
            tool_choice: decode_tool_choice(body.get("tool_choice")),
            response_format: None,
            sampling: conduit_ir::canonical::Sampling {
                temperature: body
                    .get("temperature")
                    .and_then(|v| v.as_f64())
                    .map(|v| v as f32),
                top_p: body.get("top_p").and_then(|v| v.as_f64()).map(|v| v as f32),
                max_tokens: body
                    .get("max_output_tokens")
                    .and_then(|v| v.as_u64())
                    .map(|v| v as u32),
                reasoning_effort: body
                    .pointer("/reasoning/effort")
                    .and_then(|v| v.as_str())
                    .map(str::to_string),
                service_tier: body
                    .get("service_tier")
                    .and_then(|v| v.as_str())
                    .filter(|tier| *tier == "priority")
                    .map(str::to_string),
                ..Default::default()
            },
            meta: {
                let mut meta = conduit_ir::canonical::RequestMeta {
                    user: body
                        .get("user")
                        .and_then(|v| v.as_str())
                        .map(str::to_string),
                    extra: Default::default(),
                };
                for key in [
                    "conversation_id",
                    "session_id",
                    "previous_response_id",
                ] {
                    if let Some(s) = body
                        .get(key)
                        .and_then(|v| v.as_str())
                        .map(str::trim)
                        .filter(|s| !s.is_empty())
                    {
                        meta.extra
                            .insert(key.into(), serde_json::Value::String(s.to_string()));
                    }
                }
                if let Some(m) = body.get("metadata").filter(|v| !v.is_null()) {
                    meta.extra.insert("metadata".into(), m.clone());
                }
                // P3: official Responses first-class knobs.
                for key in RESPONSES_PASSTHROUGH_KEYS {
                    if let Some(v) = body.get(*key).filter(|v| !v.is_null()) {
                        meta.extra.insert((*key).into(), v.clone());
                    }
                }
                meta
            },
            stream,
            loss_report: LossReport::default(),
        })
    }

    fn decode_response(
        body: Value,
        alias: &str,
    ) -> Result<(CanonicalChatResponse, LossReport), CodecError> {
        let mut loss = LossReport::default();
        let id = body
            .get("id")
            .and_then(|v| v.as_str())
            .unwrap_or("resp")
            .to_string();
        let model = body
            .get("model")
            .and_then(|v| v.as_str())
            .unwrap_or(alias)
            .to_string();

        let mut contents: Vec<CanonicalContent> = Vec::new();
        let mut finish = FinishReason::Stop;

        if let Some(output) = body.get("output").and_then(|o| o.as_array()) {
            for item in output {
                let ty = item.get("type").and_then(|t| t.as_str()).unwrap_or("");
                match ty {
                    "message" => {
                        if let Some(content) = item.get("content").and_then(|c| c.as_array()) {
                            for part in content {
                                let pty = part.get("type").and_then(|t| t.as_str()).unwrap_or("");
                                if pty == "output_text" || pty == "text" {
                                    if let Some(text) = part
                                        .get("text")
                                        .and_then(|t| t.as_str())
                                        .or_else(|| part.get("value").and_then(|t| t.as_str()))
                                    {
                                        contents.push(CanonicalContent::Text {
                                            text: text.to_string(),
                                        });
                                    }
                                } else {
                                    loss.add(
                                        "output[].content[].type",
                                        pty,
                                        "(dropped)",
                                        "unknown Responses message content part has no IR representation; skipped",
                                    );
                                }
                            }
                        }
                    }
                    "function_call" => {
                        let id = item
                            .get("call_id")
                            .or_else(|| item.get("id"))
                            .and_then(|v| v.as_str())
                            .unwrap_or("call")
                            .to_string();
                        let name = item
                            .get("name")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string();
                        let args_str = item
                            .get("arguments")
                            .and_then(|v| v.as_str())
                            .unwrap_or("{}");
                        let input = serde_json::from_str(args_str).unwrap_or(json!({}));
                        contents.push(CanonicalContent::ToolUse { id, name, input });
                        finish = FinishReason::ToolCalls;
                    }
                    "output_text" => {
                        if let Some(text) = item.get("text").and_then(|t| t.as_str()) {
                            contents.push(CanonicalContent::Text {
                                text: text.to_string(),
                            });
                        }
                    }
                    other => {
                        loss.add(
                            "output[].type",
                            other,
                            "(dropped)",
                            "unknown Responses output item type has no IR representation; skipped",
                        );
                    }
                }
            }
        }

        if contents.is_empty() {
            if let Some(text) = body.get("output_text").and_then(|t| t.as_str()) {
                contents.push(CanonicalContent::Text {
                    text: text.to_string(),
                });
            }
        }

        let usage = body.get("usage").map(parse_usage).unwrap_or_default();
        if let Some(status) = body.get("status").and_then(|s| s.as_str()) {
            if status == "incomplete" {
                finish = FinishReason::Length;
            }
        }

        let resp = CanonicalChatResponse {
            id,
            request_id: String::new(),
            model,
            choices: vec![CanonicalMessage {
                role: Role::Assistant,
                content: contents,
                name: None,
            }],
            finish_reason: finish,
            usage,
            created_at: chrono::Utc::now(),
        };
        Ok((resp, loss))
    }

    fn encode_response(resp: &CanonicalChatResponse) -> Value {
        let output = canonical_response_output_items(resp);
        json!({
            "id": resp.id,
            "object": "response",
            "created_at": resp.created_at.timestamp(),
            "model": resp.model,
            "status": "completed",
            "output": output,
            "usage": usage_json(&resp.usage),
        })
    }

    fn encode_chunk(chunk: &CanonicalChunk, resp_id: &str) -> (Option<String>, LossReport) {
        if let Some(BlockKind::ToolUse) = chunk.block_kind {
            if chunk.tool_use_id.is_some() || chunk.tool_name.is_some() {
                let event = json!({
                    "type": "response.output_item.added",
                    "output_index": chunk.block_index,
                    "item": {
                        "type": "function_call",
                        "id": chunk.tool_use_id.as_deref().unwrap_or(resp_id),
                        "call_id": chunk.tool_use_id.as_deref().unwrap_or(resp_id),
                        "name": chunk.tool_name.as_deref().unwrap_or(""),
                        "arguments": "",
                    }
                });
                return (Some(format!("data: {event}\n\n")), LossReport::default());
            }
        }
        if let Some(BlockDelta::TextDelta { text }) = &chunk.delta {
            let event = json!({
                "type": "response.output_text.delta",
                "delta": text,
                "output_index": chunk.block_index,
                "item_id": resp_id,
            });
            return (Some(format!("data: {event}\n\n")), LossReport::default());
        }
        if let Some(BlockDelta::InputJsonDelta { partial_json }) = &chunk.delta {
            let event = json!({
                "type": "response.function_call_arguments.delta",
                "delta": partial_json,
                "output_index": chunk.block_index,
            });
            return (Some(format!("data: {event}\n\n")), LossReport::default());
        }
        if chunk.finish_reason.is_some() {
            let event = json!({
                "type": "response.completed",
                "response": {
                    "id": resp_id,
                    "object": "response",
                    "status": "completed",
                    // Responses SDKs inspect this array at stream completion.
                    // This stateless compatibility path cannot reconstruct
                    // prior deltas, but it must never omit the field.
                    "output": [],
                    "usage": chunk.usage.as_ref().map(usage_json),
                }
            });
            return (Some(format!("data: {event}\n\n")), LossReport::default());
        }
        (None, LossReport::default())
    }

    fn decode_chunk(data: &str) -> Result<(Vec<CanonicalChunk>, LossReport), CodecError> {
        let mut state = ResponsesStreamState::default();
        Self::decode_chunk_stateful(&mut state, data)
    }

    type StreamState = ResponsesStreamState;

    fn decode_chunk_stateful(
        state: &mut Self::StreamState,
        data: &str,
    ) -> Result<(Vec<CanonicalChunk>, LossReport), CodecError> {
        let data = data.trim();
        if data.is_empty() || data == "[DONE]" {
            return Ok((vec![], LossReport::default()));
        }
        let data = data.strip_prefix("data:").map(str::trim).unwrap_or(data);
        if data.is_empty() || data == "[DONE]" {
            return Ok((vec![], LossReport::default()));
        }
        let v: Value = serde_json::from_str(data).map_err(CodecError::Serialization)?;
        Ok((
            decode_responses_sse_event_stateful(state, &v),
            LossReport::default(),
        ))
    }

    fn error_body(type_: &str, code: Option<&str>, message: &str) -> Value {
        let mut error = json!({"type": type_, "message": message});
        if let Some(c) = code {
            error["code"] = json!(c);
        }
        json!({"error": error})
    }

    fn stream_error_sse(message: &str) -> String {
        format!(
            "data: {}\n\n",
            json!({"type": "error", "error": {"message": message}})
        )
    }
}


/// Reject an output whose matching function call is absent from the adapted
/// request. The Responses adapter may restore that call from durable,
/// short-lived continuation storage before this codec runs.
fn validate_stateless_tool_outputs(items: &[Value]) -> Result<(), CodecError> {
    let mut seen_call_ids = HashSet::new();
    for item in items {
        match item.get("type").and_then(Value::as_str) {
            Some("function_call") => {
                if let Some(id) = item
                    .get("call_id")
                    .or_else(|| item.get("id"))
                    .and_then(Value::as_str)
                {
                    seen_call_ids.insert(id);
                }
            }
            Some("function_call_output") => {
                let Some(call_id) = item.get("call_id").and_then(Value::as_str) else {
                    // `decode_input_item` emits the canonical missing-field error.
                    continue;
                };
                if !seen_call_ids.contains(call_id) {
                    return Err(CodecError::InvalidValue {
                        field: "input[].call_id".into(),
                        value: format!(
                            "function_call_output `{call_id}` has no preceding function_call; provide the matching function_call in input or a live previous_response_id continuation"
                        ),
                    });
                }
            }
            _ => {}
        }
    }
    Ok(())
}

fn decode_input_item(item: &Value, messages: &mut Vec<CanonicalMessage>) -> Result<(), CodecError> {
    match item
        .get("type")
        .and_then(|v| v.as_str())
        .unwrap_or("message")
    {
        "message" => {
            let role = match item.get("role").and_then(|v| v.as_str()).unwrap_or("user") {
                "system" | "developer" => Role::System,
                "assistant" => Role::Assistant,
                _ => Role::User,
            };
            let content = decode_message_content(item.get("content"));
            messages.push(CanonicalMessage {
                role,
                content,
                name: None,
            });
        }
        "function_call" => {
            let id = item
                .get("call_id")
                .or_else(|| item.get("id"))
                .and_then(|v| v.as_str())
                .ok_or_else(|| CodecError::MissingField {
                    field: "input[].call_id".into(),
                })?
                .to_string();
            let name = item
                .get("name")
                .and_then(|v| v.as_str())
                .ok_or_else(|| CodecError::MissingField {
                    field: "input[].name".into(),
                })?
                .to_string();
            let arguments = item
                .get("arguments")
                .and_then(|v| v.as_str())
                .unwrap_or("{}");
            let input = serde_json::from_str(arguments).map_err(|e| CodecError::InvalidValue {
                field: "input[].arguments".into(),
                value: e.to_string(),
            })?;
            messages.push(CanonicalMessage {
                role: Role::Assistant,
                content: vec![CanonicalContent::ToolUse { id, name, input }],
                name: None,
            });
        }
        "function_call_output" => {
            let tool_use_id = item
                .get("call_id")
                .and_then(|v| v.as_str())
                .ok_or_else(|| CodecError::MissingField {
                    field: "input[].call_id".into(),
                })?
                .to_string();
            let content = decode_tool_output(item.get("output"));
            messages.push(CanonicalMessage {
                role: Role::Tool,
                content: vec![CanonicalContent::ToolResult {
                    tool_use_id,
                    content,
                    is_error: None,
                }],
                name: None,
            });
        }
        _ => {}
    }
    Ok(())
}

fn decode_message_content(content: Option<&Value>) -> Vec<CanonicalContent> {
    match content {
        Some(Value::String(text)) => vec![CanonicalContent::Text { text: text.clone() }],
        Some(Value::Array(parts)) => parts
            .iter()
            .filter_map(|part| match part.get("type").and_then(|v| v.as_str()) {
                Some("input_text") | Some("output_text") | Some("text") => part
                    .get("text")
                    .and_then(|v| v.as_str())
                    .map(|text| CanonicalContent::Text {
                        text: text.to_string(),
                    }),
                Some("input_image") => part.get("image_url").and_then(|v| v.as_str()).map(|url| {
                    CanonicalContent::Image {
                        url: url.to_string(),
                        media_type: None,
                        detail: part
                            .get("detail")
                            .and_then(|v| v.as_str())
                            .map(str::to_string),
                    }
                }),
                _ => None,
            })
            .collect(),
        _ => Vec::new(),
    }
}

fn decode_tool_output(output: Option<&Value>) -> Vec<CanonicalContent> {
    match output {
        Some(Value::String(text)) => vec![CanonicalContent::Text { text: text.clone() }],
        Some(Value::Array(parts)) => decode_message_content(Some(&Value::Array(parts.clone()))),
        _ => Vec::new(),
    }
}

fn decode_tool_choice(value: Option<&Value>) -> Option<ToolChoice> {
    match value {
        Some(Value::String(choice)) => Some(match choice.as_str() {
            "none" => ToolChoice::None,
            "required" => ToolChoice::Required,
            _ => ToolChoice::Auto,
        }),
        Some(Value::Object(choice))
            if choice.get("type").and_then(|v| v.as_str()) == Some("function") =>
        {
            choice
                .get("name")
                .and_then(|v| v.as_str())
                .map(|name| ToolChoice::Tool {
                    name: name.to_string(),
                })
        }
        _ => None,
    }
}

fn canonical_response_output_items(resp: &CanonicalChatResponse) -> Vec<Value> {
    let Some(choice) = resp.choices.first() else {
        return Vec::new();
    };
    let text = content_to_text(&choice.content);
    let mut output = Vec::new();
    if !text.is_empty() {
        output.push(text_item_json(
            &format!("msg_{}", resp.id),
            &text,
            "completed",
        ));
    }
    output.extend(choice.content.iter().filter_map(|content| {
        if let CanonicalContent::ToolUse { id, name, input } = content {
            Some(tool_item_json(id, name, &input.to_string(), "completed"))
        } else {
            None
        }
    }));
    output
}

