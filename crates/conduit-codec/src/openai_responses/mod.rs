//! Minimal OpenAI Responses API codec (Codex OAuth + Grok Responses).
//!
//! Wire shape (request):
//! ```json
//! { "model": "...", "input": [...], "stream": bool, "store": false, "tools": [...] }
//! ```
//!
//! ChatGPT-account Codex (`chatgpt.com/backend-api/codex`) has extra constraints —
//! see [`apply_codex_chatgpt_account_body`] (aligned with CLIProxyAPI).
//!
//! Response (non-stream): full response object with `output` array.
//! Stream: SSE events with `type` field (`response.output_text.delta`, etc.).

use conduit_ir::{
    canonical::{
        BlockDelta, BlockKind, CanonicalChatRequest, CanonicalChatResponse, CanonicalChunk,
        CanonicalContent, CanonicalMessage, FinishReason, Role, ToolChoice, Usage,
    },
    error::CodecError,
    loss::LossReport,
};
use serde_json::{json, Value};

use crate::WireCodec;

pub struct OpenAiResponsesCodec;

/// Apply CLIProxyAPI-style ChatGPT-account Codex request constraints.
///
/// Upstream rules observed on `chatgpt.com/backend-api/codex/responses`:
/// - `stream` must be `true` (non-stream clients are aggregated by the proxy)
/// - `store` must be `false`
/// - no `temperature` / `top_p` / `max_output_tokens`
/// - `instructions` present (empty string ok)
/// - `system` role → `developer`
/// - optional Codex defaults: reasoning, include encrypted content, parallel tools
pub fn apply_codex_chatgpt_account_body(mut body: Value) -> Value {
    body["stream"] = json!(true);
    body["store"] = json!(false);

    if let Some(obj) = body.as_object_mut() {
        for key in [
            "temperature",
            "top_p",
            "top_k",
            "max_output_tokens",
            "max_completion_tokens",
            "max_tokens",
            "user",
            "truncation",
            "context_management",
            "stream_options",
            "previous_response_id",
            "prompt_cache_retention",
            "safety_identifier",
        ] {
            obj.remove(key);
        }
    }

    match body.get("instructions") {
        None | Some(Value::Null) => {
            body["instructions"] = json!("");
        }
        _ => {}
    }

    if body.get("parallel_tool_calls").is_none() {
        body["parallel_tool_calls"] = json!(true);
    }
    if body.get("include").is_none() {
        body["include"] = json!(["reasoning.encrypted_content"]);
    }
    // Preserve caller-provided reasoning if already set by encode_request.
    if body.get("reasoning").is_none() {
        body["reasoning"] = json!({
            "effort": "medium",
            "summary": "auto",
        });
    } else if body.pointer("/reasoning/summary").is_none() {
        if let Some(r) = body.get_mut("reasoning").and_then(|v| v.as_object_mut()) {
            r.insert("summary".into(), json!("auto"));
        }
    }

    // Codex rejects role "system" in input — map to "developer" (CLIProxyAPI parity).
    if let Some(input) = body.get_mut("input").and_then(|v| v.as_array_mut()) {
        for item in input.iter_mut() {
            if item.get("role").and_then(|r| r.as_str()) == Some("system") {
                item["role"] = json!("developer");
            }
        }
    }

    body
}

impl WireCodec for OpenAiResponsesCodec {
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
                        if let CanonicalContent::ToolUse { id, name, input: args } = c {
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

        (body, loss)
    }

    fn decode_request(
        body: Value,
        alias: String,
        stream: bool,
        _request_id: String,
        _key_id: String,
    ) -> Result<CanonicalChatRequest, CodecError> {
        let model = body
            .get("model")
            .and_then(|m| m.as_str())
            .unwrap_or(&alias)
            .to_string();
        let mut req = CanonicalChatRequest::new(model, vec![CanonicalMessage::user("")]);
        req.stream = stream;
        Ok(req)
    }

    fn decode_response(
        body: Value,
        alias: &str,
    ) -> Result<(CanonicalChatResponse, LossReport), CodecError> {
        let loss = LossReport::default();
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
                    _ => {}
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
        let text: String = resp
            .choices
            .first()
            .map(|m| content_to_text(&m.content))
            .unwrap_or_default();
        json!({
            "id": resp.id,
            "object": "response",
            "model": resp.model,
            "status": "completed",
            "output": [{
                "type": "message",
                "role": "assistant",
                "content": [{"type": "output_text", "text": text}]
            }],
            "usage": {
                "input_tokens": resp.usage.prompt_tokens,
                "output_tokens": resp.usage.completion_tokens,
                "total_tokens": resp.usage.total_tokens,
            }
        })
    }

    fn encode_chunk(chunk: &CanonicalChunk, resp_id: &str) -> (Option<String>, LossReport) {
        if let Some(BlockDelta::TextDelta { text }) = &chunk.delta {
            let event = json!({
                "type": "response.output_text.delta",
                "delta": text,
                "item_id": resp_id,
            });
            return (Some(format!("data: {event}\n\n")), LossReport::default());
        }
        if chunk.finish_reason.is_some() {
            let event = json!({
                "type": "response.completed",
                "response": { "id": resp_id }
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

fn content_to_text(content: &[CanonicalContent]) -> String {
    content
        .iter()
        .filter_map(|c| {
            if let CanonicalContent::Text { text } = c {
                Some(text.as_str())
            } else {
                None
            }
        })
        .collect::<Vec<_>>()
        .join("")
}

/// Tracks what has already been emitted on a single Responses SSE stream so
/// terminal `response.completed` can fall back to full `output` without
/// duplicating live deltas (CLIProxyAPI HasTextDelta / HasReceivedArgumentsDelta).
#[derive(Debug, Default, Clone)]
pub struct ResponsesStreamState {
    pub saw_text: bool,
    pub saw_thinking: bool,
    /// A tool call start (id/name) was emitted.
    pub saw_tool: bool,
    /// Tool argument bytes already streamed via `function_call_arguments.delta`
    /// (or a one-shot full-args emit). Prevents re-appending complete JSON on
    /// `.done` / `output_item.done` / `completed` — that produced invalid
    /// concatenated JSON like `{"a":1}{"a":1}` → Claude "Invalid tool parameters".
    pub saw_tool_args: bool,
}

/// Decode a single Codex / OpenAI Responses SSE event into IR chunks.
///
/// Aligns with CLIProxyAPI `ConvertCodexResponseToOpenAI` /
/// `ConvertCodexResponseToClaude` event coverage.
fn decode_responses_sse_event_stateful(
    state: &mut ResponsesStreamState,
    v: &Value,
) -> Vec<CanonicalChunk> {
    let ty = v.get("type").and_then(|t| t.as_str()).unwrap_or("");
    let output_index = v
        .get("output_index")
        .and_then(|i| i.as_u64())
        .unwrap_or(0) as u32;

    match ty {
        // ── Text ──────────────────────────────────────────────────────────
        "response.output_text.delta" => {
            let text = extract_delta_text(v);
            if text.is_empty() {
                return vec![];
            }
            state.saw_text = true;
            vec![text_chunk(text, output_index)]
        }
        "response.output_text.done" => {
            // Full text only when no deltas were streamed (CLIProxyAPI pattern).
            if state.saw_text {
                return vec![];
            }
            let text = v
                .get("text")
                .and_then(|t| t.as_str())
                .unwrap_or("")
                .to_string();
            if text.is_empty() {
                return vec![];
            }
            state.saw_text = true;
            vec![text_chunk(text, output_index)]
        }

        // ── Reasoning / thinking ──────────────────────────────────────────
        "response.reasoning_summary_text.delta" => {
            let text = extract_delta_text(v);
            if text.is_empty() {
                return vec![];
            }
            state.saw_thinking = true;
            vec![thinking_chunk(text, output_index)]
        }
        "response.reasoning_summary_text.done" => {
            // Separator between summary parts (CLIProxyAPI emits "\n\n").
            if !state.saw_thinking {
                return vec![];
            }
            vec![thinking_chunk("\n\n".into(), output_index)]
        }

        // ── Tool calls ────────────────────────────────────────────────────
        "response.output_item.added" => {
            let item = match v.get("item") {
                Some(i) => i,
                None => return vec![],
            };
            let item_ty = item.get("type").and_then(|t| t.as_str()).unwrap_or("");
            match item_ty {
                "function_call" => {
                    let id = item
                        .get("call_id")
                        .or_else(|| item.get("id"))
                        .and_then(|x| x.as_str())
                        .unwrap_or("")
                        .to_string();
                    let name = item
                        .get("name")
                        .and_then(|x| x.as_str())
                        .unwrap_or("")
                        .to_string();
                    if name.is_empty() && id.is_empty() {
                        return vec![];
                    }
                    state.saw_tool = true;
                    let mut out = vec![CanonicalChunk {
                        request_id: String::new(),
                        index: 0,
                        block_index: output_index,
                        block_kind: Some(BlockKind::ToolUse),
                        delta: None,
                        finish_reason: None,
                        usage: None,
                        tool_use_id: if id.is_empty() { None } else { Some(id) },
                        tool_name: if name.is_empty() { None } else { Some(name) },
                    }];
                    // Only take complete arguments on `added` when non-empty and
                    // not just "{}". Prefer live `function_call_arguments.delta`.
                    if let Some(args) = item.get("arguments").and_then(|a| a.as_str()) {
                        let args = args.trim();
                        if !args.is_empty() && args != "{}" && !state.saw_tool_args {
                            state.saw_tool_args = true;
                            out.push(CanonicalChunk {
                                request_id: String::new(),
                                index: 0,
                                block_index: output_index,
                                block_kind: Some(BlockKind::ToolUse),
                                delta: Some(BlockDelta::InputJsonDelta {
                                    partial_json: args.to_string(),
                                }),
                                finish_reason: None,
                                usage: None,
                                tool_use_id: None,
                                tool_name: None,
                            });
                        }
                    }
                    out
                }
                "message" => {
                    // Rare: full message on added — extract output_text parts.
                    let texts = extract_message_item_texts(item);
                    if !texts.is_empty() {
                        state.saw_text = true;
                    }
                    texts
                        .into_iter()
                        .map(|t| text_chunk(t, output_index))
                        .collect()
                }
                _ => vec![],
            }
        }
        "response.function_call_arguments.delta" => {
            let args = extract_delta_text(v);
            if args.is_empty() {
                return vec![];
            }
            state.saw_tool = true;
            state.saw_tool_args = true;
            vec![CanonicalChunk {
                request_id: String::new(),
                index: 0,
                block_index: output_index,
                block_kind: Some(BlockKind::ToolUse),
                delta: Some(BlockDelta::InputJsonDelta {
                    partial_json: args,
                }),
                finish_reason: None,
                usage: None,
                tool_use_id: None,
                tool_name: None,
            }]
        }
        "response.function_call_arguments.done" => {
            // CLIProxyAPI: if deltas already streamed, emit nothing.
            if state.saw_tool_args {
                return vec![];
            }
            let args = v
                .get("arguments")
                .and_then(|a| a.as_str())
                .unwrap_or("")
                .to_string();
            if args.is_empty() {
                return vec![];
            }
            state.saw_tool = true;
            state.saw_tool_args = true;
            vec![CanonicalChunk {
                request_id: String::new(),
                index: 0,
                block_index: output_index,
                block_kind: Some(BlockKind::ToolUse),
                delta: Some(BlockDelta::InputJsonDelta {
                    partial_json: args,
                }),
                finish_reason: None,
                usage: None,
                tool_use_id: None,
                tool_name: None,
            }]
        }
        "response.output_item.done" => {
            let item = match v.get("item") {
                Some(i) => i,
                None => return vec![],
            };
            let item_ty = item.get("type").and_then(|t| t.as_str()).unwrap_or("");
            match item_ty {
                "message" => {
                    if state.saw_text {
                        return vec![];
                    }
                    let texts = extract_message_item_texts(item);
                    if !texts.is_empty() {
                        state.saw_text = true;
                    }
                    texts
                        .into_iter()
                        .map(|t| text_chunk(t, output_index))
                        .collect()
                }
                "function_call" => {
                    // Belated full tool call only if nothing was streamed yet.
                    // If start was seen but args never streamed, emit args only.
                    let id = item
                        .get("call_id")
                        .or_else(|| item.get("id"))
                        .and_then(|x| x.as_str())
                        .unwrap_or("")
                        .to_string();
                    let name = item
                        .get("name")
                        .and_then(|x| x.as_str())
                        .unwrap_or("")
                        .to_string();
                    let args = item
                        .get("arguments")
                        .and_then(|a| a.as_str())
                        .unwrap_or("")
                        .to_string();
                    let mut out = Vec::new();
                    if !state.saw_tool && (!id.is_empty() || !name.is_empty()) {
                        state.saw_tool = true;
                        out.push(CanonicalChunk {
                            request_id: String::new(),
                            index: 0,
                            block_index: output_index,
                            block_kind: Some(BlockKind::ToolUse),
                            delta: None,
                            finish_reason: None,
                            usage: None,
                            tool_use_id: if id.is_empty() { None } else { Some(id) },
                            tool_name: if name.is_empty() { None } else { Some(name) },
                        });
                    }
                    if !state.saw_tool_args && !args.is_empty() && args != "{}" {
                        state.saw_tool_args = true;
                        out.push(CanonicalChunk {
                            request_id: String::new(),
                            index: 0,
                            block_index: output_index,
                            block_kind: Some(BlockKind::ToolUse),
                            delta: Some(BlockDelta::InputJsonDelta {
                                partial_json: args,
                            }),
                            finish_reason: None,
                            usage: None,
                            tool_use_id: None,
                            tool_name: None,
                        });
                    }
                    out
                }
                "reasoning" => {
                    if state.saw_thinking {
                        return vec![];
                    }
                    // Final reasoning summary if any text was only on done.
                    let mut texts = Vec::new();
                    if let Some(summary) = item.get("summary").and_then(|s| s.as_array()) {
                        for part in summary {
                            if part.get("type").and_then(|t| t.as_str()) == Some("summary_text") {
                                if let Some(t) = part.get("text").and_then(|x| x.as_str()) {
                                    if !t.is_empty() {
                                        texts.push(t.to_string());
                                    }
                                }
                            }
                        }
                    }
                    if !texts.is_empty() {
                        state.saw_thinking = true;
                    }
                    texts
                        .into_iter()
                        .map(|t| thinking_chunk(t, output_index))
                        .collect()
                }
                _ => vec![],
            }
        }

        // ── Terminal ──────────────────────────────────────────────────────
        // Prefer content from live deltas / output_item.done. On terminal we:
        // 1) always emit finish + usage
        // 2) fall back to full `response.output` content (buffered Codex paths
        //    that never sent deltas). Downstream Anthropic encoder may show
        //    content once at the end — better than empty end_turn.
        "response.completed" | "response.done" | "response.incomplete" => {
            let response = v.get("response");
            let request_id = response
                .and_then(|r| r.get("id"))
                .and_then(|i| i.as_str())
                .unwrap_or("")
                .to_string();
            let usage = response.and_then(|r| r.get("usage")).map(parse_usage);

            let mut out: Vec<CanonicalChunk> = Vec::new();

            // Buffered / incomplete streams: recover content from terminal
            // `response.output` only when we never saw live deltas.
            if let Some(output) = response.and_then(|r| r.get("output")).and_then(|o| o.as_array())
            {
                for (idx, item) in output.iter().enumerate() {
                    let item_ty = item.get("type").and_then(|t| t.as_str()).unwrap_or("");
                    match item_ty {
                        "message" if !state.saw_text => {
                            let texts = extract_message_item_texts(item);
                            if !texts.is_empty() {
                                state.saw_text = true;
                            }
                            for t in texts {
                                out.push(text_chunk(t, idx as u32));
                            }
                        }
                        "function_call" if !state.saw_tool || !state.saw_tool_args => {
                            let id = item
                                .get("call_id")
                                .or_else(|| item.get("id"))
                                .and_then(|x| x.as_str())
                                .unwrap_or("")
                                .to_string();
                            let name = item
                                .get("name")
                                .and_then(|x| x.as_str())
                                .unwrap_or("")
                                .to_string();
                            let args = item
                                .get("arguments")
                                .and_then(|a| a.as_str())
                                .unwrap_or("")
                                .to_string();
                            if !state.saw_tool && (!id.is_empty() || !name.is_empty()) {
                                state.saw_tool = true;
                                out.push(CanonicalChunk {
                                    request_id: String::new(),
                                    index: 0,
                                    block_index: idx as u32,
                                    block_kind: Some(BlockKind::ToolUse),
                                    delta: None,
                                    finish_reason: None,
                                    usage: None,
                                    tool_use_id: if id.is_empty() { None } else { Some(id) },
                                    tool_name: if name.is_empty() {
                                        None
                                    } else {
                                        Some(name)
                                    },
                                });
                            }
                            if !state.saw_tool_args && !args.is_empty() && args != "{}" {
                                state.saw_tool_args = true;
                                out.push(CanonicalChunk {
                                    request_id: String::new(),
                                    index: 0,
                                    block_index: idx as u32,
                                    block_kind: Some(BlockKind::ToolUse),
                                    delta: Some(BlockDelta::InputJsonDelta {
                                        partial_json: args,
                                    }),
                                    finish_reason: None,
                                    usage: None,
                                    tool_use_id: None,
                                    tool_name: None,
                                });
                            }
                        }
                        "reasoning" if !state.saw_thinking => {
                            if let Some(summary) = item.get("summary").and_then(|s| s.as_array()) {
                                for part in summary {
                                    if part.get("type").and_then(|t| t.as_str())
                                        == Some("summary_text")
                                    {
                                        if let Some(t) = part.get("text").and_then(|x| x.as_str())
                                        {
                                            if !t.is_empty() {
                                                state.saw_thinking = true;
                                                out.push(thinking_chunk(
                                                    t.to_string(),
                                                    idx as u32,
                                                ));
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

            let finish_reason = if ty == "response.incomplete" {
                let reason = response
                    .and_then(|r| r.get("incomplete_details"))
                    .and_then(|d| d.get("reason"))
                    .and_then(|r| r.as_str())
                    .unwrap_or("");
                if reason.contains("max_output") || reason.contains("max_tokens") {
                    FinishReason::Length
                } else {
                    FinishReason::Other(reason.to_string())
                }
            } else if state.saw_tool {
                FinishReason::ToolCalls
            } else {
                FinishReason::Stop
            };

            out.push(CanonicalChunk {
                request_id,
                index: 0,
                block_index: 0,
                block_kind: None,
                delta: None,
                finish_reason: Some(finish_reason),
                usage,
                tool_use_id: None,
                tool_name: None,
            });
            out
        }

        // Ignore lifecycle noise (created, in_progress, content_part.*, etc.)
        _ => vec![],
    }
}

fn extract_delta_text(v: &Value) -> String {
    match v.get("delta") {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Object(map)) => map
            .get("text")
            .or_else(|| map.get("content"))
            .and_then(|t| t.as_str())
            .unwrap_or("")
            .to_string(),
        _ => v
            .get("text")
            .and_then(|t| t.as_str())
            .unwrap_or("")
            .to_string(),
    }
}

fn extract_message_item_texts(item: &Value) -> Vec<String> {
    let mut out = Vec::new();
    if let Some(content) = item.get("content").and_then(|c| c.as_array()) {
        for part in content {
            let pty = part.get("type").and_then(|t| t.as_str()).unwrap_or("");
            if pty == "output_text" || pty == "text" {
                if let Some(text) = part
                    .get("text")
                    .and_then(|t| t.as_str())
                    .or_else(|| part.get("value").and_then(|t| t.as_str()))
                {
                    if !text.is_empty() {
                        out.push(text.to_string());
                    }
                }
            }
        }
    }
    out
}

fn text_chunk(text: String, block_index: u32) -> CanonicalChunk {
    CanonicalChunk {
        request_id: String::new(),
        index: 0,
        block_index,
        block_kind: Some(BlockKind::Text),
        delta: Some(BlockDelta::TextDelta { text }),
        finish_reason: None,
        usage: None,
        tool_use_id: None,
        tool_name: None,
    }
}

fn thinking_chunk(thinking: String, block_index: u32) -> CanonicalChunk {
    CanonicalChunk {
        request_id: String::new(),
        index: 0,
        block_index,
        block_kind: Some(BlockKind::Thinking),
        delta: Some(BlockDelta::ThinkingDelta { thinking }),
        finish_reason: None,
        usage: None,
        tool_use_id: None,
        tool_name: None,
    }
}

fn parse_usage(u: &Value) -> Usage {
    let prompt = u
        .get("input_tokens")
        .or_else(|| u.get("prompt_tokens"))
        .and_then(|v| v.as_u64())
        .unwrap_or(0) as u32;
    let completion = u
        .get("output_tokens")
        .or_else(|| u.get("completion_tokens"))
        .and_then(|v| v.as_u64())
        .unwrap_or(0) as u32;
    let total = u
        .get("total_tokens")
        .and_then(|v| v.as_u64())
        .unwrap_or((prompt + completion) as u64) as u32;
    let reasoning = u
        .pointer("/output_tokens_details/reasoning_tokens")
        .or_else(|| u.pointer("/completion_tokens_details/reasoning_tokens"))
        .and_then(|v| v.as_u64())
        .unwrap_or(0) as u32;
    let cache_read = u
        .pointer("/input_tokens_details/cached_tokens")
        .and_then(|v| v.as_u64())
        .unwrap_or(0) as u32;
    let cache_write = u
        .pointer("/input_tokens_details/cache_write_tokens")
        .and_then(|v| v.as_u64())
        .unwrap_or(0) as u32;
    Usage {
        prompt_tokens: prompt,
        completion_tokens: completion,
        total_tokens: total,
        reasoning_tokens: reasoning,
        cache_read_tokens: cache_read,
        cache_write_tokens: cache_write,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_simple_request() {
        let req = CanonicalChatRequest::new(
            "gpt-5",
            vec![
                CanonicalMessage::system("sys"),
                CanonicalMessage::user("hello"),
            ],
        );
        let (wire, _) = OpenAiResponsesCodec::encode_request(&req, false);
        assert_eq!(wire["model"], "gpt-5");
        assert!(wire["input"].as_array().unwrap().len() >= 2);
        assert_eq!(wire["stream"], false);
        assert_eq!(wire["store"], false);
    }

    #[test]
    fn codex_chatgpt_body_forces_stream_store_and_strips_sampling() {
        let req = CanonicalChatRequest::new(
            "gpt-5.5",
            vec![
                CanonicalMessage::system("be helpful"),
                CanonicalMessage::user("hi"),
            ],
        );
        let mut req = req;
        req.sampling.temperature = Some(0.2);
        req.sampling.max_tokens = Some(64);
        let (wire, _) = OpenAiResponsesCodec::encode_request(&req, false);
        let wire = apply_codex_chatgpt_account_body(wire);
        assert_eq!(wire["stream"], true);
        assert_eq!(wire["store"], false);
        assert_eq!(wire["instructions"], "");
        assert!(wire.get("temperature").is_none());
        assert!(wire.get("max_output_tokens").is_none());
        assert_eq!(wire["input"][0]["role"], "developer");
        assert_eq!(wire["parallel_tool_calls"], true);
        assert!(wire["include"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v.as_str() == Some("reasoning.encrypted_content")));
    }

    #[test]
    fn decode_message_response() {
        let body = json!({
            "id": "resp_1",
            "model": "gpt-5",
            "status": "completed",
            "output": [{
                "type": "message",
                "role": "assistant",
                "content": [{"type": "output_text", "text": "hi there"}]
            }],
            "usage": {"input_tokens": 3, "output_tokens": 2, "total_tokens": 5}
        });
        let (resp, _) = OpenAiResponsesCodec::decode_response(body, "gpt-5").unwrap();
        assert_eq!(resp.id, "resp_1");
        assert_eq!(content_to_text(&resp.choices[0].content), "hi there");
        assert_eq!(resp.usage.total_tokens, 5);
    }

    #[test]
    fn decode_text_delta_chunk() {
        let data = r#"{"type":"response.output_text.delta","delta":"Hello"}"#;
        let (chunks, _) = OpenAiResponsesCodec::decode_chunk(data).unwrap();
        assert_eq!(chunks.len(), 1);
        match &chunks[0].delta {
            Some(BlockDelta::TextDelta { text }) => assert_eq!(text, "Hello"),
            other => panic!("unexpected delta: {other:?}"),
        }
    }

    #[test]
    fn decode_reasoning_summary_delta() {
        let data = r#"{"type":"response.reasoning_summary_text.delta","delta":"plan"}"#;
        let (chunks, _) = OpenAiResponsesCodec::decode_chunk(data).unwrap();
        assert!(matches!(
            &chunks[0].delta,
            Some(BlockDelta::ThinkingDelta { thinking }) if thinking == "plan"
        ));
    }

    #[test]
    fn decode_function_call_added_and_args() {
        let mut st = ResponsesStreamState::default();
        let added = r#"{"type":"response.output_item.added","output_index":0,"item":{"type":"function_call","call_id":"c1","name":"search"}}"#;
        let (chunks, _) =
            OpenAiResponsesCodec::decode_chunk_stateful(&mut st, added).unwrap();
        assert_eq!(chunks[0].tool_use_id.as_deref(), Some("c1"));
        assert_eq!(chunks[0].tool_name.as_deref(), Some("search"));

        let args = r#"{"type":"response.function_call_arguments.delta","output_index":0,"delta":"{\"q\":1}"}"#;
        let (chunks, _) =
            OpenAiResponsesCodec::decode_chunk_stateful(&mut st, args).unwrap();
        assert!(matches!(
            &chunks[0].delta,
            Some(BlockDelta::InputJsonDelta { partial_json }) if partial_json.contains("q")
        ));
    }

    /// Repro Claude "Invalid tool parameters": streaming args then full JSON again
    /// produced `{"a":1}{"a":1}`.
    #[test]
    fn decode_tool_args_not_duplicated_after_deltas() {
        let mut st = ResponsesStreamState::default();
        let _ = OpenAiResponsesCodec::decode_chunk_stateful(
            &mut st,
            r#"{"type":"response.output_item.added","output_index":0,"item":{"type":"function_call","call_id":"c1","name":"Bash","arguments":""}}"#,
        )
        .unwrap();
        for part in [r#"{"#, r#""command":"pwd""#, r#"}"#] {
            let ev = format!(
                r#"{{"type":"response.function_call_arguments.delta","output_index":0,"delta":{}}}"#,
                serde_json::to_string(part).unwrap()
            );
            let (chunks, _) =
                OpenAiResponsesCodec::decode_chunk_stateful(&mut st, &ev).unwrap();
            assert_eq!(chunks.len(), 1);
        }
        // .done with full args must be ignored
        let (chunks, _) = OpenAiResponsesCodec::decode_chunk_stateful(
            &mut st,
            r#"{"type":"response.function_call_arguments.done","output_index":0,"arguments":"{\"command\":\"pwd\"}"}"#,
        )
        .unwrap();
        assert!(chunks.is_empty(), "done must not re-emit args: {chunks:?}");

        // output_item.done must not re-emit
        let (chunks, _) = OpenAiResponsesCodec::decode_chunk_stateful(
            &mut st,
            r#"{"type":"response.output_item.done","output_index":0,"item":{"type":"function_call","call_id":"c1","name":"Bash","arguments":"{\"command\":\"pwd\"}"}}"#,
        )
        .unwrap();
        assert!(
            chunks.is_empty(),
            "output_item.done must not re-emit: {chunks:?}"
        );

        // completed must not re-emit tool args
        let (chunks, _) = OpenAiResponsesCodec::decode_chunk_stateful(
            &mut st,
            r#"{"type":"response.completed","response":{"id":"r1","usage":{"input_tokens":1,"output_tokens":1,"total_tokens":2},"output":[{"type":"function_call","call_id":"c1","name":"Bash","arguments":"{\"command\":\"pwd\"}"}]}}"#,
        )
        .unwrap();
        assert!(
            !chunks.iter().any(|c| matches!(
                &c.delta,
                Some(BlockDelta::InputJsonDelta { .. })
            )),
            "completed must not re-emit args: {chunks:?}"
        );

        // Simulate Anthropic encoder path: concat all arg deltas only once
        let mut st2 = ResponsesStreamState::default();
        let mut acc = String::new();
        let events = [
            r#"{"type":"response.output_item.added","output_index":0,"item":{"type":"function_call","call_id":"c1","name":"Bash"}}"#,
            r#"{"type":"response.function_call_arguments.delta","delta":"{\"command\":\"pwd\"}"}"#,
            r#"{"type":"response.function_call_arguments.done","arguments":"{\"command\":\"pwd\"}"}"#,
            r#"{"type":"response.completed","response":{"id":"r1","output":[{"type":"function_call","call_id":"c1","name":"Bash","arguments":"{\"command\":\"pwd\"}"}],"usage":{"input_tokens":1,"output_tokens":1,"total_tokens":2}}}"#,
        ];
        for ev in events {
            let (chunks, _) =
                OpenAiResponsesCodec::decode_chunk_stateful(&mut st2, ev).unwrap();
            for c in chunks {
                if let Some(BlockDelta::InputJsonDelta { partial_json }) = c.delta {
                    acc.push_str(&partial_json);
                }
            }
        }
        assert_eq!(acc, r#"{"command":"pwd"}"#);
        assert!(serde_json::from_str::<serde_json::Value>(&acc).is_ok());
    }

    /// Buffered Codex: only terminal event with full output (terra symptom).
    #[test]
    fn decode_completed_recovers_message_when_no_deltas() {
        let data = r#"{
            "type":"response.completed",
            "response":{
                "id":"resp_x",
                "usage":{"input_tokens":10,"output_tokens":5,"total_tokens":15},
                "output":[
                    {"type":"reasoning","summary":[{"type":"summary_text","text":"think hard"}]},
                    {"type":"message","content":[{"type":"output_text","text":"hello terra"}]}
                ]
            }
        }"#;
        let (chunks, _) = OpenAiResponsesCodec::decode_chunk(data).unwrap();
        assert!(
            chunks.iter().any(|c| matches!(
                &c.delta,
                Some(BlockDelta::ThinkingDelta { thinking }) if thinking == "think hard"
            )),
            "expected thinking from completed.output: {chunks:?}"
        );
        assert!(
            chunks.iter().any(|c| matches!(
                &c.delta,
                Some(BlockDelta::TextDelta { text }) if text == "hello terra"
            )),
            "expected text from completed.output: {chunks:?}"
        );
        assert!(chunks
            .iter()
            .any(|c| c.finish_reason == Some(FinishReason::Stop)));
    }

    #[test]
    fn decode_completed_skips_text_if_already_streamed() {
        let mut st = ResponsesStreamState::default();
        let delta = r#"{"type":"response.output_text.delta","delta":"hi"}"#;
        let _ = OpenAiResponsesCodec::decode_chunk_stateful(&mut st, delta).unwrap();
        let done = r#"{
            "type":"response.completed",
            "response":{
                "id":"resp_x",
                "usage":{"input_tokens":1,"output_tokens":1,"total_tokens":2},
                "output":[{"type":"message","content":[{"type":"output_text","text":"hi DUPLICATE"}]}]
            }
        }"#;
        let (chunks, _) =
            OpenAiResponsesCodec::decode_chunk_stateful(&mut st, done).unwrap();
        assert!(
            !chunks.iter().any(|c| matches!(
                &c.delta,
                Some(BlockDelta::TextDelta { text }) if text.contains("DUPLICATE")
            )),
            "must not re-emit terminal text after live deltas: {chunks:?}"
        );
        assert!(chunks
            .iter()
            .any(|c| c.finish_reason == Some(FinishReason::Stop)));
    }

    #[test]
    fn encode_user_tool_result_as_function_call_output() {
        let msg = CanonicalMessage {
            role: Role::User,
            content: vec![
                CanonicalContent::ToolResult {
                    tool_use_id: "c1".into(),
                    content: vec![CanonicalContent::Text {
                        text: "ok".into(),
                    }],
                    is_error: None,
                },
                CanonicalContent::Text {
                    text: "continue".into(),
                },
            ],
            name: None,
        };
        let req = CanonicalChatRequest::new("gpt-5.6-terra", vec![msg]);
        let (wire, _) = OpenAiResponsesCodec::encode_request(&req, true);
        let input = wire["input"].as_array().unwrap();
        assert_eq!(input[0]["type"], "function_call_output");
        assert_eq!(input[0]["call_id"], "c1");
        assert_eq!(input[1]["role"], "user");
    }
}
