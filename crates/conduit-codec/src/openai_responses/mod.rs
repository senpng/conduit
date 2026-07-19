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
        CanonicalContent, CanonicalMessage, FinishReason, Role, ToolChoice, ToolDef, Usage,
    },
    error::CodecError,
    loss::LossReport,
};
use std::collections::{BTreeMap, HashSet};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::WireCodec;

pub struct OpenAiResponsesCodec;

/// Durable state required to expand a Responses `previous_response_id` turn.
///
/// This is deliberately protocol-only data. Persistence, tenancy, and expiry
/// policies are supplied by the hosting gateway.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ResponsesContinuation {
    pub input_items: Vec<Value>,
    pub output_items: Vec<Value>,
}

/// Result of inspecting a client Responses request for continuation handling.
pub enum ResponsesContinuationRequest {
    /// The request is self-contained (or supplied a complete replacement
    /// transcript), so it can be decoded immediately.
    Ready(Value),
    /// The gateway must load this response id before decoding the request.
    Incremental {
        previous_response_id: String,
        body: Value,
    },
}

impl ResponsesContinuation {
    pub fn new(input: Value, output_items: Vec<Value>) -> Self {
        Self {
            input_items: responses_input_items(input),
            output_items,
        }
    }

    pub fn from_json(input_items_json: &str, output_items_json: &str) -> serde_json::Result<Self> {
        Ok(Self {
            input_items: serde_json::from_str(input_items_json)?,
            output_items: serde_json::from_str(output_items_json)?,
        })
    }

    pub fn input_items_json(&self) -> serde_json::Result<String> {
        serde_json::to_string(&self.input_items)
    }

    pub fn output_items_json(&self) -> serde_json::Result<String> {
        serde_json::to_string(&self.output_items)
    }
}

/// Inspect a Responses request before decoding it. Full transcripts are kept as
/// supplied; incremental turns request a continuation lookup.
pub fn prepare_responses_continuation(body: Value) -> ResponsesContinuationRequest {
    let previous_response_id = body
        .get("previous_response_id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .map(str::to_string);
    let Some(previous_response_id) = previous_response_id else {
        return ResponsesContinuationRequest::Ready(body);
    };
    if responses_input_contains_full_transcript(&responses_input_items_from_body(&body)) {
        return ResponsesContinuationRequest::Ready(remove_previous_response_id(body));
    }
    ResponsesContinuationRequest::Incremental {
        previous_response_id,
        body,
    }
}

/// Merge a persisted Responses transcript into an incremental request.
pub fn merge_responses_continuation(body: Value, continuation: &ResponsesContinuation) -> Value {
    let mut items = continuation.input_items.clone();
    items.extend(continuation.output_items.clone());
    items.extend(responses_input_items_from_body(&body));
    let mut body = remove_previous_response_id(body);
    body["input"] = Value::Array(dedupe_response_function_calls(items));
    body
}

/// Whether an incremental request can safely start a fresh continuation when
/// its referenced response has expired. Tool outputs cannot: their matching
/// calls are required by the upstream request.
pub fn can_reset_responses_continuation(body: &Value) -> bool {
    !responses_input_items_from_body(body).iter().any(|item| {
        matches!(
            response_item_type(item),
            "function_call_output" | "custom_tool_call_output"
        )
    })
}

/// Remove an unusable response id before forwarding a fresh Responses turn.
pub fn reset_responses_continuation(body: Value) -> Value {
    remove_previous_response_id(body)
}

/// Whether this client permits the Responses server to retain continuation
/// state. The protocol default is `true` when the field is absent.
pub fn responses_store_enabled(body: &Value) -> bool {
    body.get("store").and_then(Value::as_bool).unwrap_or(true)
}

/// Read a complete Responses response object's output items.
pub fn response_output_items(response: &Value) -> Vec<Value> {
    response
        .get("output")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}

/// Stateful encoder for client-facing Responses API SSE streams.
///
/// The Responses protocol is an item lifecycle, not merely a sequence of text
/// deltas. In particular, SDKs reconstruct the final response from the
/// `response.completed.response.output` array. Keep that array present even
/// for an empty response, and emit the message/tool item lifecycle around
/// deltas so strict Responses clients can consume the stream.
#[derive(Debug)]
pub struct ResponsesStreamEncoder {
    response_id: String,
    model: String,
    store: bool,
    started: bool,
    next_output_index: u32,
    text_item: Option<usize>,
    tools: BTreeMap<u32, usize>,
    output: Vec<StreamOutputItem>,
    completed: bool,
}

#[derive(Debug)]
enum StreamOutputItem {
    Text {
        output_index: u32,
        id: String,
        text: String,
        started: bool,
        closed: bool,
    },
    Tool {
        output_index: u32,
        id: String,
        name: String,
        arguments: String,
        started: bool,
        closed: bool,
    },
}

impl ResponsesStreamEncoder {
    pub fn new(response_id: impl Into<String>, model: impl Into<String>) -> Self {
        Self::new_with_store(response_id, model, true)
    }

    pub fn new_with_store(
        response_id: impl Into<String>,
        model: impl Into<String>,
        store: bool,
    ) -> Self {
        Self {
            response_id: response_id.into(),
            model: model.into(),
            store,
            started: false,
            next_output_index: 0,
            text_item: None,
            tools: BTreeMap::new(),
            output: Vec::new(),
            completed: false,
        }
    }

    /// Emit the initial Responses lifecycle events. Calling this repeatedly is
    /// safe; only the first call produces frames.
    pub fn start(&mut self) -> Vec<String> {
        if self.started {
            return vec![];
        }
        self.started = true;
        let response = self.response_json("in_progress", vec![], None);
        vec![
            sse(json!({"type": "response.created", "response": response.clone()})),
            sse(json!({"type": "response.in_progress", "response": response})),
        ]
    }

    /// Encode one canonical chunk into zero or more Responses SSE frames.
    pub fn push(&mut self, chunk: &CanonicalChunk) -> Vec<String> {
        if self.completed {
            return vec![];
        }

        let mut out = self.start();

        if let Some(BlockDelta::TextDelta { text }) = &chunk.delta {
            if !text.is_empty() {
                let item = self.ensure_text_item(&mut out);
                let (output_index, item_id) = match &mut self.output[item] {
                    StreamOutputItem::Text {
                        output_index,
                        id,
                        text: accumulated,
                        ..
                    } => {
                        accumulated.push_str(text);
                        (*output_index, id.clone())
                    }
                    StreamOutputItem::Tool { .. } => unreachable!("text item has text shape"),
                };
                out.push(sse(json!({
                    "type": "response.output_text.delta",
                    "output_index": output_index,
                    "content_index": 0,
                    "item_id": item_id,
                    "delta": text,
                })));
            }
        }

        let is_tool = matches!(chunk.block_kind, Some(BlockKind::ToolUse))
            || chunk.tool_use_id.is_some()
            || chunk.tool_name.is_some()
            || matches!(chunk.delta, Some(BlockDelta::InputJsonDelta { .. }));
        if is_tool {
            let item = self.ensure_tool_item(chunk.block_index);
            if let StreamOutputItem::Tool {
                id,
                name,
                arguments,
                ..
            } = &mut self.output[item]
            {
                if let Some(tool_id) = &chunk.tool_use_id {
                    if !tool_id.is_empty() {
                        *id = tool_id.clone();
                    }
                }
                if let Some(tool_name) = &chunk.tool_name {
                    if !tool_name.is_empty() {
                        *name = tool_name.clone();
                    }
                }
                if let Some(BlockDelta::InputJsonDelta { partial_json }) = &chunk.delta {
                    arguments.push_str(partial_json);
                }
            }
            self.start_tool_item(item, &mut out);

            if let Some(BlockDelta::InputJsonDelta { partial_json }) = &chunk.delta {
                if !partial_json.is_empty() {
                    let (output_index, item_id) = match &self.output[item] {
                        StreamOutputItem::Tool {
                            output_index, id, ..
                        } => (*output_index, id.clone()),
                        StreamOutputItem::Text { .. } => unreachable!("tool item has tool shape"),
                    };
                    out.push(sse(json!({
                        "type": "response.function_call_arguments.delta",
                        "output_index": output_index,
                        "item_id": item_id,
                        "delta": partial_json,
                    })));
                }
            }
        }

        if let Some(reason) = &chunk.finish_reason {
            let usage = chunk.usage.as_ref();
            self.complete(reason, usage, &mut out);
        }

        out
    }

    /// Complete an otherwise unterminated stream. This is mainly useful for
    /// consumers that observe an EOF without a canonical terminal chunk.
    pub fn finish(&mut self) -> Vec<String> {
        if self.completed {
            return vec![];
        }
        let mut out = self.start();
        self.complete(&FinishReason::Stop, None, &mut out);
        out
    }

    /// Completed Responses output items, suitable for durable continuation
    /// replay after the stream's terminal event.
    pub fn output_items(&self) -> Vec<Value> {
        self.output.iter().map(StreamOutputItem::as_json).collect()
    }

    fn ensure_text_item(&mut self, out: &mut Vec<String>) -> usize {
        if let Some(item) = self.text_item {
            return item;
        }
        let output_index = self.allocate_output_index();
        let id = format!("msg_{}", self.response_id);
        let item = self.output.len();
        self.output.push(StreamOutputItem::Text {
            output_index,
            id: id.clone(),
            text: String::new(),
            started: true,
            closed: false,
        });
        self.text_item = Some(item);
        out.push(sse(json!({
            "type": "response.output_item.added",
            "output_index": output_index,
            "item": {
                "id": id,
                "type": "message",
                "status": "in_progress",
                "role": "assistant",
                "content": [],
            },
        })));
        out.push(sse(json!({
            "type": "response.content_part.added",
            "output_index": output_index,
            "item_id": format!("msg_{}", self.response_id),
            "content_index": 0,
            "part": {"type": "output_text", "text": "", "annotations": []},
        })));
        item
    }

    fn ensure_tool_item(&mut self, block_index: u32) -> usize {
        if let Some(&item) = self.tools.get(&block_index) {
            return item;
        }
        let output_index = self.allocate_output_index();
        let item = self.output.len();
        self.output.push(StreamOutputItem::Tool {
            output_index,
            id: String::new(),
            name: String::new(),
            arguments: String::new(),
            started: false,
            closed: false,
        });
        self.tools.insert(block_index, item);
        item
    }

    fn start_tool_item(&mut self, item: usize, out: &mut Vec<String>) {
        let payload = match &mut self.output[item] {
            StreamOutputItem::Tool {
                output_index,
                id,
                name,
                arguments,
                started,
                ..
            } if !*started && !id.is_empty() && !name.is_empty() => {
                *started = true;
                Some((*output_index, id.clone(), name.clone(), arguments.clone()))
            }
            _ => None,
        };
        if let Some((output_index, id, name, arguments)) = payload {
            out.push(sse(json!({
                "type": "response.output_item.added",
                "output_index": output_index,
                "item": {
                    "id": id,
                    "type": "function_call",
                    "status": "in_progress",
                    "call_id": id,
                    "name": name,
                    "arguments": arguments,
                },
            })));
        }
    }

    fn complete(&mut self, _reason: &FinishReason, usage: Option<&Usage>, out: &mut Vec<String>) {
        if self.completed {
            return;
        }
        for item in &mut self.output {
            match item {
                StreamOutputItem::Text {
                    output_index,
                    id,
                    text,
                    started,
                    closed,
                } if *started && !*closed => {
                    let part = output_text_part(text);
                    out.push(sse(json!({
                        "type": "response.output_text.done",
                        "output_index": *output_index,
                        "content_index": 0,
                        "item_id": id,
                        "text": text,
                    })));
                    out.push(sse(json!({
                        "type": "response.content_part.done",
                        "output_index": *output_index,
                        "item_id": id,
                        "content_index": 0,
                        "part": part,
                    })));
                    out.push(sse(json!({
                        "type": "response.output_item.done",
                        "output_index": *output_index,
                        "item": text_item_json(id, text, "completed"),
                    })));
                    *closed = true;
                }
                StreamOutputItem::Tool {
                    output_index,
                    id,
                    name,
                    arguments,
                    started,
                    closed,
                } if *started && !*closed => {
                    out.push(sse(json!({
                        "type": "response.function_call_arguments.done",
                        "output_index": *output_index,
                        "item_id": id,
                        "arguments": arguments,
                    })));
                    out.push(sse(json!({
                        "type": "response.output_item.done",
                        "output_index": *output_index,
                        "item": tool_item_json(id, name, arguments, "completed"),
                    })));
                    *closed = true;
                }
                _ => {}
            }
        }
        let output = self.output.iter().map(StreamOutputItem::as_json).collect();
        let response = self.response_json("completed", output, usage);
        out.push(sse(
            json!({"type": "response.completed", "response": response}),
        ));
        self.completed = true;
    }

    fn allocate_output_index(&mut self) -> u32 {
        let index = self.next_output_index;
        self.next_output_index = self.next_output_index.saturating_add(1);
        index
    }

    fn response_json(&self, status: &str, output: Vec<Value>, usage: Option<&Usage>) -> Value {
        json!({
            "id": self.response_id,
            "object": "response",
            "model": self.model,
            "status": status,
            "store": self.store,
            "output": output,
            "usage": usage.map(usage_json),
        })
    }
}

impl StreamOutputItem {
    fn as_json(&self) -> Value {
        match self {
            Self::Text { id, text, .. } => text_item_json(id, text, "completed"),
            Self::Tool {
                id,
                name,
                arguments,
                ..
            } => tool_item_json(id, name, arguments, "completed"),
        }
    }
}

fn sse(event: Value) -> String {
    format!("data: {event}\n\n")
}

fn output_text_part(text: &str) -> Value {
    json!({"type": "output_text", "text": text, "annotations": []})
}

fn text_item_json(id: &str, text: &str, status: &str) -> Value {
    json!({
        "id": id,
        "type": "message",
        "status": status,
        "role": "assistant",
        "content": [output_text_part(text)],
    })
}

fn tool_item_json(id: &str, name: &str, arguments: &str, status: &str) -> Value {
    json!({
        "id": id,
        "type": "function_call",
        "status": status,
        "call_id": id,
        "name": name,
        "arguments": arguments,
    })
}

fn usage_json(usage: &Usage) -> Value {
    json!({
        "input_tokens": usage.prompt_tokens,
        "output_tokens": usage.completion_tokens,
        "total_tokens": usage.total_tokens,
    })
}

fn responses_input_items_from_body(body: &Value) -> Vec<Value> {
    body.get("input")
        .cloned()
        .map(responses_input_items)
        .unwrap_or_default()
}

fn responses_input_items(input: Value) -> Vec<Value> {
    match input {
        Value::Array(items) => items,
        Value::String(text) => vec![json!({
            "type": "message",
            "role": "user",
            "content": [{"type": "input_text", "text": text}],
        })],
        _ => Vec::new(),
    }
}

fn responses_input_contains_full_transcript(input: &[Value]) -> bool {
    input.iter().any(|item| {
        matches!(
            response_item_type(item),
            "function_call" | "custom_tool_call"
        ) || (response_item_type(item) == "message"
            && item.get("role").and_then(Value::as_str) == Some("assistant"))
    })
}

fn dedupe_response_function_calls(items: Vec<Value>) -> Vec<Value> {
    let mut seen = HashSet::new();
    items
        .into_iter()
        .filter(|item| {
            if response_item_type(item) != "function_call" {
                return true;
            }
            let Some(call_id) = item
                .get("call_id")
                .or_else(|| item.get("id"))
                .and_then(Value::as_str)
            else {
                return true;
            };
            seen.insert(call_id.to_string())
        })
        .collect()
}

fn remove_previous_response_id(mut body: Value) -> Value {
    if let Some(object) = body.as_object_mut() {
        object.remove("previous_response_id");
    }
    body
}

fn response_item_type(item: &Value) -> &str {
    item.get("type").and_then(Value::as_str).unwrap_or_default()
}

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
            "model": resp.model,
            "status": "completed",
            "output": output,
            "usage": {
                "input_tokens": resp.usage.prompt_tokens,
                "output_tokens": resp.usage.completion_tokens,
                "total_tokens": resp.usage.total_tokens,
            }
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
                    "usage": chunk.usage.as_ref().map(|usage| json!({
                        "input_tokens": usage.prompt_tokens,
                        "output_tokens": usage.completion_tokens,
                        "total_tokens": usage.total_tokens,
                    })),
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
    let output_index = v.get("output_index").and_then(|i| i.as_u64()).unwrap_or(0) as u32;

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
                        block_index: output_index,
                        block_kind: Some(BlockKind::ToolUse),
                        tool_use_id: if id.is_empty() { None } else { Some(id) },
                        tool_name: if name.is_empty() { None } else { Some(name) },
                        ..Default::default()
                    }];
                    // Only take complete arguments on `added` when non-empty and
                    // not just "{}". Prefer live `function_call_arguments.delta`.
                    if let Some(args) = item.get("arguments").and_then(|a| a.as_str()) {
                        let args = args.trim();
                        if !args.is_empty() && args != "{}" && !state.saw_tool_args {
                            state.saw_tool_args = true;
                            out.push(CanonicalChunk {
                                block_index: output_index,
                                block_kind: Some(BlockKind::ToolUse),
                                delta: Some(BlockDelta::InputJsonDelta {
                                    partial_json: args.to_string(),
                                }),
                                ..Default::default()
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
                block_index: output_index,
                block_kind: Some(BlockKind::ToolUse),
                delta: Some(BlockDelta::InputJsonDelta { partial_json: args }),
                ..Default::default()
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
                block_index: output_index,
                block_kind: Some(BlockKind::ToolUse),
                delta: Some(BlockDelta::InputJsonDelta { partial_json: args }),
                ..Default::default()
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
                            block_index: output_index,
                            block_kind: Some(BlockKind::ToolUse),
                            tool_use_id: if id.is_empty() { None } else { Some(id) },
                            tool_name: if name.is_empty() { None } else { Some(name) },
                            ..Default::default()
                        });
                    }
                    if !state.saw_tool_args && !args.is_empty() && args != "{}" {
                        state.saw_tool_args = true;
                        out.push(CanonicalChunk {
                            block_index: output_index,
                            block_kind: Some(BlockKind::ToolUse),
                            delta: Some(BlockDelta::InputJsonDelta { partial_json: args }),
                            ..Default::default()
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
            if let Some(output) = response
                .and_then(|r| r.get("output"))
                .and_then(|o| o.as_array())
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
                                    block_index: idx as u32,
                                    block_kind: Some(BlockKind::ToolUse),
                                    tool_use_id: if id.is_empty() { None } else { Some(id) },
                                    tool_name: if name.is_empty() { None } else { Some(name) },
                                    ..Default::default()
                                });
                            }
                            if !state.saw_tool_args && !args.is_empty() && args != "{}" {
                                state.saw_tool_args = true;
                                out.push(CanonicalChunk {
                                    block_index: idx as u32,
                                    block_kind: Some(BlockKind::ToolUse),
                                    delta: Some(BlockDelta::InputJsonDelta { partial_json: args }),
                                    ..Default::default()
                                });
                            }
                        }
                        "reasoning" if !state.saw_thinking => {
                            if let Some(summary) = item.get("summary").and_then(|s| s.as_array()) {
                                for part in summary {
                                    if part.get("type").and_then(|t| t.as_str())
                                        == Some("summary_text")
                                    {
                                        if let Some(t) = part.get("text").and_then(|x| x.as_str()) {
                                            if !t.is_empty() {
                                                state.saw_thinking = true;
                                                out.push(thinking_chunk(t.to_string(), idx as u32));
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
                finish_reason: Some(finish_reason),
                usage,
                ..Default::default()
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
    fn decode_request_preserves_input_tools_and_reasoning() {
        let body = json!({
            "model": "gpt-5",
            "instructions": "Be concise.",
            "input": [
                {"type": "message", "role": "user", "content": [
                    {"type": "input_text", "text": "Find the answer"}
                ]},
                {"type": "function_call", "call_id": "call_1", "name": "search", "arguments": "{\"q\":\"conduit\"}"},
                {"type": "function_call_output", "call_id": "call_1", "output": "found it"}
            ],
            "tools": [{
                "type": "function",
                "name": "search",
                "description": "Search documents",
                "parameters": {"type": "object"}
            }],
            "tool_choice": {"type": "function", "name": "search"},
            "reasoning": {"effort": "high"},
            "max_output_tokens": 128,
            "service_tier": "priority",
            "stream": true
        });
        let req = OpenAiResponsesCodec::decode_request(
            body,
            "gpt-5".into(),
            true,
            "req_1".into(),
            "key_1".into(),
        )
        .expect("Responses request must decode");

        assert_eq!(req.id, "req_1");
        assert_eq!(req.messages.len(), 4);
        assert_eq!(req.messages[0].role, Role::System);
        assert_eq!(req.messages[1].role, Role::User);
        assert!(matches!(
            req.messages[2].content.as_slice(),
            [CanonicalContent::ToolUse { id, name, .. }] if id == "call_1" && name == "search"
        ));
        assert!(matches!(
            req.messages[3].content.as_slice(),
            [CanonicalContent::ToolResult { tool_use_id, .. }] if tool_use_id == "call_1"
        ));
        assert_eq!(req.tools[0].name, "search");
        assert!(matches!(
            req.tool_choice,
            Some(ToolChoice::Tool { ref name }) if name == "search"
        ));
        assert_eq!(req.sampling.reasoning_effort.as_deref(), Some("high"));
        assert_eq!(req.sampling.max_tokens, Some(128));
        assert_eq!(req.sampling.service_tier.as_deref(), Some("priority"));
        assert!(req.stream);
    }

    #[test]
    fn decode_request_rejects_orphaned_tool_output_for_stateless_gateway() {
        let body = json!({
            "model": "gpt-5",
            "previous_response_id": "resp_old",
            "input": [{
                "type": "function_call_output",
                "call_id": "call_1",
                "output": "found it"
            }]
        });

        let error = OpenAiResponsesCodec::decode_request(
            body,
            "gpt-5".into(),
            false,
            "req_1".into(),
            "key_1".into(),
        )
        .expect_err("unresolved tool output must be rejected locally");
        assert!(error.to_string().contains("has no preceding function_call"));
    }

    #[test]
    fn continuation_merges_plain_text_history_in_protocol_layer() {
        let continuation = ResponsesContinuation::new(
            json!([{"type": "message", "role": "user", "content": "remember this"}]),
            vec![json!({
                "type": "message",
                "role": "assistant",
                "content": [{"type": "output_text", "text": "I remember"}]
            })],
        );
        let request = json!({
            "previous_response_id": "resp_1",
            "input": "what did I ask you to remember?"
        });
        let ResponsesContinuationRequest::Incremental { body, .. } =
            prepare_responses_continuation(request)
        else {
            panic!("expected incremental request");
        };
        let merged = merge_responses_continuation(body, &continuation);
        let input = merged["input"].as_array().unwrap();
        assert_eq!(input.len(), 3);
        assert_eq!(input[0]["content"], "remember this");
        assert_eq!(input[1]["role"], "assistant");
        assert_eq!(
            input[2]["content"][0]["text"],
            "what did I ask you to remember?"
        );
        assert!(merged.get("previous_response_id").is_none());
    }

    #[test]
    fn expired_continuation_can_reset_only_without_tool_output() {
        let plain = json!({"previous_response_id": "resp_old", "input": "continue"});
        assert!(can_reset_responses_continuation(&plain));
        assert!(reset_responses_continuation(plain)
            .get("previous_response_id")
            .is_none());

        let tool_output = json!({
            "previous_response_id": "resp_old",
            "input": [{"type": "function_call_output", "call_id": "call_1", "output": "x"}]
        });
        assert!(!can_reset_responses_continuation(&tool_output));
    }

    #[test]
    fn store_defaults_to_true_and_is_reflected_in_stream_events() {
        assert!(responses_store_enabled(&json!({})));
        assert!(!responses_store_enabled(&json!({"store": false})));

        let mut encoder = ResponsesStreamEncoder::new_with_store("resp_1", "gpt-test", false);
        let created = encoder.start().remove(0);
        assert!(created.contains("\"store\":false"));
    }

    #[test]
    fn encode_stream_emits_responses_tool_and_terminal_events() {
        let tool_start = CanonicalChunk {
            block_index: 0,
            block_kind: Some(BlockKind::ToolUse),
            tool_use_id: Some("call_1".into()),
            tool_name: Some("search".into()),
            ..Default::default()
        };
        let tool_frame = OpenAiResponsesCodec::encode_chunk(&tool_start, "resp_1")
            .0
            .expect("tool start must emit an SSE frame");
        assert!(tool_frame.contains("response.output_item.added"));
        assert!(tool_frame.contains("call_1"));

        let terminal = CanonicalChunk {
            finish_reason: Some(FinishReason::Stop),
            usage: Some(Usage {
                prompt_tokens: 2,
                completion_tokens: 3,
                total_tokens: 5,
                ..Default::default()
            }),
            ..Default::default()
        };
        let terminal_frame = OpenAiResponsesCodec::encode_chunk(&terminal, "resp_1")
            .0
            .expect("terminal chunk must emit an SSE frame");
        assert!(terminal_frame.contains("response.completed"));
        assert!(terminal_frame.contains("\"total_tokens\":5"));
        assert!(terminal_frame.contains("\"output\":[]"));
    }

    #[test]
    fn stateful_stream_emits_text_lifecycle_and_completed_output() {
        let mut enc = ResponsesStreamEncoder::new("resp_1", "gpt-test");
        let mut frames = enc.start();
        frames.extend(enc.push(&CanonicalChunk::text_delta("hello")));
        frames.extend(enc.push(&CanonicalChunk::finish(
            FinishReason::Stop,
            Some(Usage {
                prompt_tokens: 2,
                completion_tokens: 1,
                total_tokens: 3,
                ..Default::default()
            }),
        )));

        let payloads: Vec<Value> = frames
            .iter()
            .map(|frame| {
                serde_json::from_str(
                    frame
                        .strip_prefix("data: ")
                        .unwrap()
                        .trim_end_matches("\n\n"),
                )
                .unwrap()
            })
            .collect();
        assert!(payloads
            .iter()
            .any(|v| v["type"] == "response.output_item.added"));
        assert!(payloads
            .iter()
            .any(|v| v["type"] == "response.content_part.added"));
        let completed = payloads
            .iter()
            .find(|v| v["type"] == "response.completed")
            .expect("terminal event");
        assert_eq!(completed["response"]["output"].as_array().unwrap().len(), 1);
        assert_eq!(
            completed["response"]["output"][0]["content"][0]["text"],
            "hello"
        );
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
        let (chunks, _) = OpenAiResponsesCodec::decode_chunk_stateful(&mut st, added).unwrap();
        assert_eq!(chunks[0].tool_use_id.as_deref(), Some("c1"));
        assert_eq!(chunks[0].tool_name.as_deref(), Some("search"));

        let args = r#"{"type":"response.function_call_arguments.delta","output_index":0,"delta":"{\"q\":1}"}"#;
        let (chunks, _) = OpenAiResponsesCodec::decode_chunk_stateful(&mut st, args).unwrap();
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
            let (chunks, _) = OpenAiResponsesCodec::decode_chunk_stateful(&mut st, &ev).unwrap();
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
            !chunks
                .iter()
                .any(|c| matches!(&c.delta, Some(BlockDelta::InputJsonDelta { .. }))),
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
            let (chunks, _) = OpenAiResponsesCodec::decode_chunk_stateful(&mut st2, ev).unwrap();
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
        let (chunks, _) = OpenAiResponsesCodec::decode_chunk_stateful(&mut st, done).unwrap();
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
                    content: vec![CanonicalContent::Text { text: "ok".into() }],
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
