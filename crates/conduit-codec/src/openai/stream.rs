use conduit_ir::{
    canonical::{BlockDelta, BlockKind, CanonicalChunk, FinishReason, Usage},
    error::CodecError,
};
use serde_json::Value;

use super::decode_response::{decode_finish_reason, decode_usage, encode_chat_usage};

/// Decode a single OpenAI SSE `data:` line into zero or more canonical chunks.
///
/// One upstream frame may yield multiple IR chunks (e.g. content + finish_reason,
/// or reasoning + content + tool_calls). Returns an empty vec for `[DONE]` /
/// blank / ping lines.
///
/// Also accepts **non-streaming** `chat.completion` objects delivered as a single
/// SSE frame (common with some terra/proxy channels that ignore `stream:true` or
/// buffer until complete). In that case content is read from `choices[0].message`.
pub fn decode_chunks(data: &str) -> Result<Vec<CanonicalChunk>, CodecError> {
    let data = data.trim();
    if data.is_empty() || data == "[DONE]" {
        return Ok(vec![]);
    }

    // Strip optional leading `data:` if a caller passed a full SSE line.
    let data = data.strip_prefix("data:").map(str::trim).unwrap_or(data);
    if data.is_empty() || data == "[DONE]" {
        return Ok(vec![]);
    }

    let val: Value = serde_json::from_str(data)?;
    decode_openai_payload(&val)
}

fn decode_openai_payload(val: &Value) -> Result<Vec<CanonicalChunk>, CodecError> {
    let choice = &val["choices"][0];

    // Usage-only chunk (stream_options.include_usage final frame)
    if choice.is_null()
        || (choice.is_object() && choice.as_object().map(|m| m.is_empty()).unwrap_or(true))
    {
        if !val["usage"].is_null() {
            let usage = decode_usage(&val["usage"]);
            if usage != Usage::default() {
                return Ok(vec![usage_only_chunk(usage)]);
            }
        }
        return Ok(vec![]);
    }

    let mut out = Vec::new();

    // Prefer streaming `delta`; fall back to non-stream `message` on the same wire.
    let delta = &choice["delta"];
    let message = &choice["message"];
    let has_delta = delta.is_object() && delta.as_object().map(|m| !m.is_empty()).unwrap_or(false);
    let has_message =
        message.is_object() && message.as_object().map(|m| !m.is_empty()).unwrap_or(false);

    let content_source = if has_delta { delta } else { message };

    // reasoning_content / reasoning / reasoning_text
    for text in collect_reasoning_texts(content_source) {
        if text.is_empty() {
            continue;
        }
        out.push(CanonicalChunk::thinking_delta(text));
    }

    // Text content (string, array of parts, or null)
    for text in extract_text_contents(&content_source["content"]) {
        if text.is_empty() {
            continue;
        }
        out.push(CanonicalChunk::text_delta(text));
    }

    // Some providers put assistant text in `text` (legacy) or `output_text`
    if out.iter().all(|c| {
        !matches!(
            c.delta,
            Some(BlockDelta::TextDelta { .. }) | Some(BlockDelta::ThinkingDelta { .. })
        )
    }) {
        for key in ["text", "output_text"] {
            if let Some(t) = content_source[key].as_str() {
                if !t.is_empty() {
                    out.push(CanonicalChunk::text_delta(t.to_string()));
                    break;
                }
            }
        }
    }

    // Tool calls from delta or message
    let tool_calls = content_source
        .get("tool_calls")
        .and_then(|v| v.as_array())
        .or_else(|| {
            // Non-stream sometimes nests under message only; already using content_source
            None
        });

    if let Some(tcs) = tool_calls {
        for tc in tcs {
            emit_tool_call_chunks(tc, &mut out);
        }
    }

    // If delta was present but empty of content, and message also has payload
    // (rare hybrid frames), merge message as well.
    if has_delta && has_message {
        let msg_only_tools = message.get("tool_calls").and_then(|v| v.as_array());
        if let Some(tcs) = msg_only_tools {
            // Only if delta had no tool_calls
            if delta.get("tool_calls").is_none() {
                for tc in tcs {
                    emit_tool_call_chunks(tc, &mut out);
                }
            }
        }
        if extract_text_contents(&delta["content"]).is_empty() {
            for text in extract_text_contents(&message["content"]) {
                if text.is_empty() {
                    continue;
                }
                out.push(CanonicalChunk::text_delta(text));
            }
        }
    }

    // finish_reason (may share the frame with content/tool_calls — keep both)
    if let Some(reason_str) = choice["finish_reason"].as_str() {
        if !reason_str.is_empty() {
            let finish_reason = decode_finish_reason(Some(reason_str));
            let usage = if !val["usage"].is_null() {
                let u = decode_usage(&val["usage"]);
                if u != Usage::default() {
                    Some(u)
                } else {
                    None
                }
            } else {
                None
            };
            out.push(CanonicalChunk::finish(finish_reason, usage));
        }
    } else if !val["usage"].is_null() && out.is_empty() {
        let usage = decode_usage(&val["usage"]);
        if usage != Usage::default() {
            out.push(usage_only_chunk(usage));
        }
    }

    Ok(out)
}

fn usage_only_chunk(usage: Usage) -> CanonicalChunk {
    CanonicalChunk {
        usage: Some(usage),
        ..Default::default()
    }
}

fn emit_tool_call_chunks(tc: &Value, out: &mut Vec<CanonicalChunk>) {
    let block_index = tc["index"].as_u64().unwrap_or(0) as u32;
    let id = tc["id"].as_str().filter(|s| !s.is_empty());
    let name = tc["function"]["name"]
        .as_str()
        .or_else(|| tc["name"].as_str())
        .filter(|s| !s.is_empty());

    // Non-stream tool_calls often have full arguments as a complete JSON string.
    let args = tc["function"]["arguments"]
        .as_str()
        .or_else(|| tc["arguments"].as_str())
        .unwrap_or("");

    if id.is_some() || name.is_some() {
        out.push(CanonicalChunk {
            block_index,
            block_kind: Some(BlockKind::ToolUse),
            tool_use_id: id.map(str::to_string),
            tool_name: name.map(str::to_string),
            ..Default::default()
        });
    }

    if !args.is_empty() {
        out.push(CanonicalChunk {
            block_index,
            block_kind: Some(BlockKind::ToolUse),
            delta: Some(BlockDelta::InputJsonDelta {
                partial_json: args.to_string(),
            }),
            ..Default::default()
        });
    }
}

/// Extract text strings from OpenAI content field (string | array | null).
fn extract_text_contents(content: &Value) -> Vec<String> {
    match content {
        Value::Null => vec![],
        Value::String(s) => {
            if s.is_empty() {
                vec![]
            } else {
                vec![s.clone()]
            }
        }
        Value::Array(parts) => {
            let mut out = Vec::new();
            for part in parts {
                match part {
                    Value::String(s) if !s.is_empty() => out.push(s.clone()),
                    Value::Object(map) => {
                        let ty = map.get("type").and_then(|t| t.as_str()).unwrap_or("text");
                        if ty == "text" || ty == "output_text" {
                            if let Some(t) = map.get("text").and_then(|t| t.as_str()) {
                                if !t.is_empty() {
                                    out.push(t.to_string());
                                }
                            }
                        }
                    }
                    _ => {}
                }
            }
            out
        }
        _ => vec![],
    }
}

/// Back-compat: first chunk only (prefer [`decode_chunks`]).
pub fn decode_chunk(data: &str) -> Result<Option<CanonicalChunk>, CodecError> {
    Ok(decode_chunks(data)?.into_iter().next())
}

fn collect_reasoning_texts(delta: &Value) -> Vec<String> {
    let mut texts = Vec::new();
    for key in [
        "reasoning_content",
        "reasoning",
        "reasoning_text",
        "thinking",
    ] {
        if let Some(node) = delta.get(key) {
            collect_reasoning_node(node, &mut texts);
        }
    }
    texts
}

fn collect_reasoning_node(node: &Value, out: &mut Vec<String>) {
    match node {
        Value::String(s) if !s.is_empty() => out.push(s.clone()),
        Value::Array(arr) => {
            for item in arr {
                collect_reasoning_node(item, out);
            }
        }
        Value::Object(map) => {
            if let Some(Value::String(s)) = map.get("text") {
                if !s.is_empty() {
                    out.push(s.clone());
                }
            } else if let Some(Value::String(s)) = map.get("content") {
                if !s.is_empty() {
                    out.push(s.clone());
                }
            }
        }
        _ => {}
    }
}

/// Stateful OpenAI Chat Completions SSE encoder (CLIProxyAPI parity).
///
/// - Fixes `created` once for the whole stream (not per-chunk `now()`).
/// - Stamps `id` / `object` / `model` on every chunk.
/// - Emits a role-only first frame (`delta.role = "assistant"`) before content.
/// - Ends with `data: [DONE]` via [`Self::finish`].
pub struct OpenAIStreamEncoder {
    resp_id: String,
    model: String,
    /// Unix seconds, captured at stream open and reused for every frame.
    created: i64,
    role_sent: bool,
    finished: bool,
}

impl OpenAIStreamEncoder {
    pub fn new(resp_id: impl Into<String>, model: impl Into<String>) -> Self {
        Self {
            resp_id: resp_id.into(),
            model: model.into(),
            created: chrono::Utc::now().timestamp(),
            role_sent: false,
            finished: false,
        }
    }

    /// Encode one IR chunk into zero or more SSE frames.
    pub fn push(&mut self, chunk: &CanonicalChunk) -> Vec<String> {
        if self.finished {
            return vec![];
        }
        let Some(payload) = encode_chunk_inner(chunk, &self.resp_id, &self.model, self.created)
        else {
            return vec![];
        };

        let mut out = Vec::with_capacity(2);
        // CLIProxyAPI message_start equivalent: role kickoff before first content.
        if !self.role_sent {
            out.push(self.role_frame());
            self.role_sent = true;
        }
        out.push(payload);
        out
    }

    /// Terminal OpenAI stream sentinel (CLIProxyAPI handler parity).
    pub fn finish(&mut self) -> Vec<String> {
        if self.finished {
            return vec![];
        }
        self.finished = true;
        // Ensure clients that require a role frame still get one on empty streams.
        let mut out = Vec::new();
        if !self.role_sent {
            out.push(self.role_frame());
            self.role_sent = true;
        }
        out.push("data: [DONE]\n\n".to_string());
        out
    }

    pub fn created(&self) -> i64 {
        self.created
    }

    fn role_frame(&self) -> String {
        let v = serde_json::json!({
            "id": self.resp_id,
            "object": "chat.completion.chunk",
            "created": self.created,
            "model": self.model,
            "choices": [{
                "index": 0,
                "delta": {"role": "assistant"},
                "finish_reason": null
            }]
        });
        format!("data: {v}\n\n")
    }
}

/// Encode a canonical chunk into an OpenAI SSE `data: {...}\n\n` line.
///
/// Stateless helper (each call uses `Utc::now()` for `created`). Prefer
/// [`OpenAIStreamEncoder`] on the gateway hot path for fixed `created` + role.
pub fn encode_chunk(chunk: &CanonicalChunk, resp_id: &str) -> Option<String> {
    encode_chunk_with_model(chunk, resp_id, "")
}

/// Like [`encode_chunk`] but stamps the client-facing model / route alias.
pub fn encode_chunk_with_model(
    chunk: &CanonicalChunk,
    resp_id: &str,
    model: &str,
) -> Option<String> {
    encode_chunk_inner(chunk, resp_id, model, chrono::Utc::now().timestamp())
}

fn encode_chunk_inner(
    chunk: &CanonicalChunk,
    resp_id: &str,
    model: &str,
    created: i64,
) -> Option<String> {
    let base = serde_json::json!({
        "id": resp_id,
        "object": "chat.completion.chunk",
        // Required by strict OpenAI-compatible deserializers (e.g. Grok Build).
        "created": created,
        "model": model,
    });

    match chunk {
        CanonicalChunk {
            delta: Some(BlockDelta::TextDelta { text }),
            ..
        } => {
            let mut v = base.clone();
            v["choices"] = serde_json::json!([{
                "index": 0,
                "delta": {"content": text},
                "finish_reason": null
            }]);
            Some(format!("data: {}\n\n", v))
        }

        CanonicalChunk {
            delta: Some(BlockDelta::ThinkingDelta { thinking }),
            ..
        } => {
            let mut v = base.clone();
            v["choices"] = serde_json::json!([{
                "index": 0,
                "delta": {"reasoning_content": thinking},
                "finish_reason": null
            }]);
            Some(format!("data: {}\n\n", v))
        }

        CanonicalChunk {
            block_kind: Some(BlockKind::ToolUse),
            tool_use_id: Some(id),
            tool_name: Some(name),
            block_index,
            delta: None,
            ..
        } => {
            let mut v = base.clone();
            v["choices"] = serde_json::json!([{
                "index": 0,
                "delta": {
                    "tool_calls": [{
                        "index": block_index,
                        "id": id,
                        "type": "function",
                        "function": {"name": name, "arguments": ""},
                    }]
                },
                "finish_reason": null
            }]);
            Some(format!("data: {}\n\n", v))
        }

        CanonicalChunk {
            delta: Some(BlockDelta::InputJsonDelta { partial_json }),
            block_index,
            ..
        } => {
            let mut v = base.clone();
            v["choices"] = serde_json::json!([{
                "index": 0,
                "delta": {
                    "tool_calls": [{
                        "index": block_index,
                        "function": {"arguments": partial_json},
                    }]
                },
                "finish_reason": null
            }]);
            Some(format!("data: {}\n\n", v))
        }

        CanonicalChunk {
            finish_reason: Some(fr),
            usage,
            ..
        } => {
            let fr_str = finish_reason_to_str(fr);
            let mut v = base.clone();
            let choice = serde_json::json!({
                "index": 0,
                "delta": {},
                "finish_reason": fr_str
            });
            if let Some(u) = usage {
                v["usage"] = encode_chat_usage(u);
            }
            v["choices"] = serde_json::json!([choice]);
            Some(format!("data: {}\n\n", v))
        }

        CanonicalChunk {
            usage: Some(u),
            finish_reason: None,
            ..
        } => {
            // Official stream_options.include_usage final frame: empty choices +
            // full CompletionUsage (same shape as non-stream).
            let mut v = base.clone();
            v["choices"] = serde_json::json!([]);
            v["usage"] = encode_chat_usage(u);
            Some(format!("data: {}\n\n", v))
        }

        _ => None,
    }
}

fn finish_reason_to_str(fr: &FinishReason) -> String {
    match fr {
        FinishReason::Stop => "stop".into(),
        FinishReason::Length => "length".into(),
        FinishReason::ToolCalls => "tool_calls".into(),
        FinishReason::ContentFilter => "content_filter".into(),
        // Preserve provider-specific reasons (parity with non-stream encode_response).
        FinishReason::Other(s) => s.clone(),
        _ => "stop".into(),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::anthropic::stream::AnthropicStreamEncoder;

    #[test]
    fn encode_chunk_includes_created_and_model() {
        let chunk = CanonicalChunk::text_delta("hi");
        let sse = encode_chunk_with_model(&chunk, "chatcmpl_test", "grok-4.5").unwrap();
        assert!(sse.starts_with("data: "));
        let data = sse.trim_start_matches("data: ").trim();
        let v: serde_json::Value = serde_json::from_str(data).unwrap();
        assert_eq!(v["object"], "chat.completion.chunk");
        assert_eq!(v["id"], "chatcmpl_test");
        assert_eq!(v["model"], "grok-4.5");
        assert!(
            v.get("created").and_then(|c| c.as_i64()).is_some(),
            "OpenAI clients require `created` on every chunk: {v}"
        );
        assert_eq!(v["choices"][0]["delta"]["content"], "hi");
    }

    #[test]
    fn stream_encoder_fixes_created_emits_role_and_done() {
        let mut enc = OpenAIStreamEncoder::new("chatcmpl_1", "grok-4.5");
        let created = enc.created();

        let frames = enc.push(&CanonicalChunk::text_delta("hi"));
        assert_eq!(frames.len(), 2, "role kickoff + content");

        let role: serde_json::Value =
            serde_json::from_str(frames[0].trim_start_matches("data: ").trim()).unwrap();
        assert_eq!(role["choices"][0]["delta"]["role"], "assistant");
        assert_eq!(role["created"], created);
        assert_eq!(role["model"], "grok-4.5");
        assert_eq!(role["id"], "chatcmpl_1");

        let content: serde_json::Value =
            serde_json::from_str(frames[1].trim_start_matches("data: ").trim()).unwrap();
        assert_eq!(content["choices"][0]["delta"]["content"], "hi");
        assert_eq!(content["created"], created, "created must be fixed for stream");
        assert!(content["choices"][0]["delta"].get("role").is_none());

        let more = enc.push(&CanonicalChunk::text_delta("!"));
        assert_eq!(more.len(), 1, "no second role frame");
        let more_v: serde_json::Value =
            serde_json::from_str(more[0].trim_start_matches("data: ").trim()).unwrap();
        assert_eq!(more_v["created"], created);

        let fin = enc.finish();
        assert_eq!(fin, vec!["data: [DONE]\n\n".to_string()]);
        assert!(enc.finish().is_empty(), "finish is idempotent");
    }

    #[test]
    fn done_returns_empty() {
        assert!(decode_chunks("[DONE]").unwrap().is_empty());
    }

    #[test]
    fn blank_returns_empty() {
        assert!(decode_chunks("").unwrap().is_empty());
    }

    #[test]
    fn text_delta() {
        let data = r#"{"id":"c1","object":"chat.completion.chunk","choices":[{"index":0,"delta":{"content":"hello"},"finish_reason":null}]}"#;
        let chunks = decode_chunks(data).unwrap();
        assert_eq!(chunks.len(), 1);
        assert!(
            matches!(chunks[0].delta, Some(BlockDelta::TextDelta { ref text }) if text == "hello")
        );
    }

    #[test]
    fn finish_reason_stop() {
        let data = r#"{"id":"c1","object":"chat.completion.chunk","choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}"#;
        let chunks = decode_chunks(data).unwrap();
        assert_eq!(chunks[0].finish_reason, Some(FinishReason::Stop));
    }

    #[test]
    fn content_and_finish_same_frame() {
        let data = r#"{"id":"c1","object":"chat.completion.chunk","choices":[{"index":0,"delta":{"content":"bye"},"finish_reason":"stop"}]}"#;
        let chunks = decode_chunks(data).unwrap();
        assert_eq!(chunks.len(), 2);
        assert!(matches!(
            chunks[0].delta,
            Some(BlockDelta::TextDelta { ref text }) if text == "bye"
        ));
        assert_eq!(chunks[1].finish_reason, Some(FinishReason::Stop));
    }

    #[test]
    fn reasoning_content_delta() {
        let data = r#"{"id":"c1","choices":[{"index":0,"delta":{"reasoning_content":"think"},"finish_reason":null}]}"#;
        let chunks = decode_chunks(data).unwrap();
        assert_eq!(chunks.len(), 1);
        assert!(matches!(
            chunks[0].delta,
            Some(BlockDelta::ThinkingDelta { ref thinking }) if thinking == "think"
        ));
    }

    #[test]
    fn tool_call_start() {
        let data = r#"{"id":"c1","object":"chat.completion.chunk","choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"call_1","type":"function","function":{"name":"search","arguments":""}}]},"finish_reason":null}]}"#;
        let chunks = decode_chunks(data).unwrap();
        assert_eq!(chunks[0].tool_use_id.as_deref(), Some("call_1"));
        assert_eq!(chunks[0].tool_name.as_deref(), Some("search"));
    }

    #[test]
    fn tool_call_argument_delta() {
        let data = r#"{"id":"c1","object":"chat.completion.chunk","choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"arguments":"{\"q\":"}}]},"finish_reason":null}]}"#;
        let chunks = decode_chunks(data).unwrap();
        assert!(
            matches!(chunks[0].delta, Some(BlockDelta::InputJsonDelta { ref partial_json }) if partial_json == "{\"q\":")
        );
    }

    #[test]
    fn tool_call_start_and_args_same_frame() {
        let data = r#"{"id":"c1","choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"c1","type":"function","function":{"name":"f","arguments":"{"}}]},"finish_reason":null}]}"#;
        let chunks = decode_chunks(data).unwrap();
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0].tool_use_id.as_deref(), Some("c1"));
        assert!(matches!(
            chunks[1].delta,
            Some(BlockDelta::InputJsonDelta { .. })
        ));
    }

    /// Reproduces the gpt-5.6-terra symptom: non-stream body as one SSE frame →
    /// previously only finish_reason was seen (no content_block_*).
    #[test]
    fn non_stream_message_shape_on_sse_emits_text_and_finish() {
        let data = r#"{
            "id":"chatcmpl_1",
            "object":"chat.completion",
            "choices":[{
                "index":0,
                "message":{"role":"assistant","content":"Hello from terra"},
                "finish_reason":"stop"
            }],
            "usage":{"prompt_tokens":18616,"completion_tokens":108,"total_tokens":18724}
        }"#;
        let chunks = decode_chunks(data).unwrap();
        assert!(
            chunks.iter().any(|c| matches!(
                &c.delta,
                Some(BlockDelta::TextDelta { text }) if text == "Hello from terra"
            )),
            "expected text from message.content, got {chunks:?}"
        );
        assert!(
            chunks
                .iter()
                .any(|c| c.finish_reason == Some(FinishReason::Stop)),
            "expected finish_reason"
        );

        // Full Anthropic client path must show content_block_start/delta/stop.
        let mut enc = AnthropicStreamEncoder::new("msg_1", "gpt-5.6-terra");
        let mut joined = String::new();
        for c in &chunks {
            for frame in enc.push(c) {
                joined.push_str(&frame);
            }
        }
        for frame in enc.finish() {
            joined.push_str(&frame);
        }
        assert!(joined.contains("content_block_start"), "{joined}");
        assert!(joined.contains("Hello from terra"), "{joined}");
        assert!(joined.contains("content_block_stop"), "{joined}");
        assert!(joined.contains("message_stop"), "{joined}");
    }

    #[test]
    fn content_array_parts() {
        let data = r#"{"choices":[{"delta":{"content":[{"type":"text","text":"partA"}]},"finish_reason":null}]}"#;
        let chunks = decode_chunks(data).unwrap();
        assert!(matches!(
            &chunks[0].delta,
            Some(BlockDelta::TextDelta { text }) if text == "partA"
        ));
    }

    #[test]
    fn non_stream_tool_calls_on_message() {
        let data = r#"{
            "choices":[{
                "message":{
                    "role":"assistant",
                    "content":null,
                    "tool_calls":[{
                        "id":"call_x",
                        "type":"function",
                        "function":{"name":"read","arguments":"{\"p\":1}"}
                    }]
                },
                "finish_reason":"tool_calls"
            }]
        }"#;
        let chunks = decode_chunks(data).unwrap();
        assert!(chunks
            .iter()
            .any(|c| c.tool_use_id.as_deref() == Some("call_x")));
        assert!(chunks.iter().any(|c| matches!(
            &c.delta,
            Some(BlockDelta::InputJsonDelta { partial_json }) if partial_json.contains("\"p\"")
        )));
        assert_eq!(
            chunks.last().and_then(|c| c.finish_reason.clone()),
            Some(FinishReason::ToolCalls)
        );
    }

    #[test]
    fn stream_usage_only_frame_includes_details() {
        // Official stream_options.include_usage final chunk: empty choices + full usage.
        let usage = Usage {
            prompt_tokens: 100,
            completion_tokens: 20,
            total_tokens: 120,
            reasoning_tokens: 8,
            cache_read_tokens: 60,
            cache_write_tokens: 15,
        };
        let sse = encode_chunk_with_model(
            &CanonicalChunk {
                usage: Some(usage),
                ..Default::default()
            },
            "chatcmpl_u",
            "gpt-4o",
        )
        .unwrap();
        let v: serde_json::Value =
            serde_json::from_str(sse.trim_start_matches("data: ").trim()).unwrap();
        assert_eq!(v["choices"], serde_json::json!([]));
        assert_eq!(v["usage"]["prompt_tokens"], 100);
        assert_eq!(v["usage"]["completion_tokens"], 20);
        assert_eq!(v["usage"]["total_tokens"], 120);
        assert_eq!(v["usage"]["prompt_tokens_details"]["cached_tokens"], 60);
        assert_eq!(
            v["usage"]["prompt_tokens_details"]["cache_write_tokens"],
            15
        );
        assert_eq!(
            v["usage"]["completion_tokens_details"]["reasoning_tokens"],
            8
        );
    }

    #[test]
    fn stream_finish_with_usage_includes_details() {
        let sse = encode_chunk_with_model(
            &CanonicalChunk::finish(
                FinishReason::Stop,
                Some(Usage {
                    prompt_tokens: 10,
                    completion_tokens: 5,
                    total_tokens: 15,
                    reasoning_tokens: 2,
                    cache_read_tokens: 4,
                    cache_write_tokens: 0,
                }),
            ),
            "chatcmpl_f",
            "o3",
        )
        .unwrap();
        let v: serde_json::Value =
            serde_json::from_str(sse.trim_start_matches("data: ").trim()).unwrap();
        assert_eq!(v["choices"][0]["finish_reason"], "stop");
        assert_eq!(v["usage"]["prompt_tokens_details"]["cached_tokens"], 4);
        assert!(v["usage"]["prompt_tokens_details"]
            .get("cache_write_tokens")
            .is_none());
        assert_eq!(
            v["usage"]["completion_tokens_details"]["reasoning_tokens"],
            2
        );
    }

    #[test]
    fn stream_finish_preserves_other_reason() {
        let sse = encode_chunk_with_model(
            &CanonicalChunk::finish(FinishReason::Other("network_error".into()), None),
            "chatcmpl_o",
            "m",
        )
        .unwrap();
        let v: serde_json::Value =
            serde_json::from_str(sse.trim_start_matches("data: ").trim()).unwrap();
        assert_eq!(v["choices"][0]["finish_reason"], "network_error");
    }

    #[test]
    fn stream_usage_omits_details_when_zero() {
        let sse = encode_chunk_with_model(
            &CanonicalChunk {
                usage: Some(Usage {
                    prompt_tokens: 1,
                    completion_tokens: 1,
                    total_tokens: 2,
                    ..Default::default()
                }),
                ..Default::default()
            },
            "chatcmpl_z",
            "m",
        )
        .unwrap();
        let v: serde_json::Value =
            serde_json::from_str(sse.trim_start_matches("data: ").trim()).unwrap();
        assert!(v["usage"].get("prompt_tokens_details").is_none());
        assert!(v["usage"].get("completion_tokens_details").is_none());
    }
}
