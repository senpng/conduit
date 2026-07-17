use conduit_ir::{
    canonical::{BlockDelta, BlockKind, CanonicalChunk, FinishReason, Usage},
    error::CodecError,
};
use serde_json::Value;

use super::decode_response::{decode_finish_reason, decode_usage};

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
        out.push(CanonicalChunk {
            request_id: String::new(),
            index: 0,
            block_index: 0,
            block_kind: Some(BlockKind::Thinking),
            delta: Some(BlockDelta::ThinkingDelta { thinking: text }),
            finish_reason: None,
            usage: None,
            tool_use_id: None,
            tool_name: None,
        });
    }

    // Text content (string, array of parts, or null)
    for text in extract_text_contents(&content_source["content"]) {
        if text.is_empty() {
            continue;
        }
        out.push(CanonicalChunk {
            request_id: String::new(),
            index: 0,
            block_index: 0,
            block_kind: Some(BlockKind::Text),
            delta: Some(BlockDelta::TextDelta { text }),
            finish_reason: None,
            usage: None,
            tool_use_id: None,
            tool_name: None,
        });
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
                    out.push(CanonicalChunk {
                        request_id: String::new(),
                        index: 0,
                        block_index: 0,
                        block_kind: Some(BlockKind::Text),
                        delta: Some(BlockDelta::TextDelta {
                            text: t.to_string(),
                        }),
                        finish_reason: None,
                        usage: None,
                        tool_use_id: None,
                        tool_name: None,
                    });
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
                out.push(CanonicalChunk {
                    request_id: String::new(),
                    index: 0,
                    block_index: 0,
                    block_kind: Some(BlockKind::Text),
                    delta: Some(BlockDelta::TextDelta { text }),
                    finish_reason: None,
                    usage: None,
                    tool_use_id: None,
                    tool_name: None,
                });
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
            out.push(CanonicalChunk {
                request_id: String::new(),
                index: 0,
                block_index: 0,
                block_kind: None,
                delta: None,
                finish_reason: Some(finish_reason),
                usage,
                tool_use_id: None,
                tool_name: None,
            });
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
        request_id: String::new(),
        index: 0,
        block_index: 0,
        block_kind: None,
        delta: None,
        finish_reason: None,
        usage: Some(usage),
        tool_use_id: None,
        tool_name: None,
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
            request_id: String::new(),
            index: 0,
            block_index,
            block_kind: Some(BlockKind::ToolUse),
            delta: None,
            finish_reason: None,
            usage: None,
            tool_use_id: id.map(str::to_string),
            tool_name: name.map(str::to_string),
        });
    }

    if !args.is_empty() {
        out.push(CanonicalChunk {
            request_id: String::new(),
            index: 0,
            block_index,
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

/// Encode a canonical chunk into an OpenAI SSE `data: {...}\n\n` line.
pub fn encode_chunk(chunk: &CanonicalChunk, resp_id: &str) -> Option<String> {
    let base = serde_json::json!({
        "id": resp_id,
        "object": "chat.completion.chunk",
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
                v["usage"] = serde_json::json!({
                    "prompt_tokens": u.prompt_tokens,
                    "completion_tokens": u.completion_tokens,
                    "total_tokens": u.total_tokens,
                });
                let _ = choice;
            }
            v["choices"] = serde_json::json!([choice]);
            Some(format!("data: {}\n\n", v))
        }

        CanonicalChunk {
            usage: Some(u),
            finish_reason: None,
            ..
        } => {
            let mut v = base.clone();
            v["choices"] = serde_json::json!([]);
            v["usage"] = serde_json::json!({
                "prompt_tokens": u.prompt_tokens,
                "completion_tokens": u.completion_tokens,
                "total_tokens": u.total_tokens,
            });
            Some(format!("data: {}\n\n", v))
        }

        _ => None,
    }
}

fn finish_reason_to_str(fr: &FinishReason) -> &'static str {
    match fr {
        FinishReason::Stop => "stop",
        FinishReason::Length => "length",
        FinishReason::ToolCalls => "tool_calls",
        FinishReason::ContentFilter => "content_filter",
        FinishReason::Other(_) => "stop",
        _ => "stop",
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
        assert!(chunks.iter().any(|c| c.tool_use_id.as_deref() == Some("call_x")));
        assert!(chunks.iter().any(|c| matches!(
            &c.delta,
            Some(BlockDelta::InputJsonDelta { partial_json }) if partial_json.contains("\"p\"")
        )));
        assert_eq!(
            chunks.last().and_then(|c| c.finish_reason.clone()),
            Some(FinishReason::ToolCalls)
        );
    }
}
