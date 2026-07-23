use conduit_ir::{
    canonical::{
        CanonicalChatResponse, CanonicalContent, CanonicalMessage, FinishReason, Role, Usage,
    },
    error::CodecError,
    loss::LossReport,
};
use serde_json::Value;

/// Decode an OpenAI `/v1/chat/completions` response body into the canonical IR.
///
/// The returned [`LossReport`] records response data that the IR cannot
/// represent (e.g. extra `choices` beyond the first, or a `refusal` message),
/// so LossReport reflects what was dropped instead of losing it silently.
pub fn decode_response(
    body: Value,
    alias: &str,
) -> Result<(CanonicalChatResponse, LossReport), CodecError> {
    let mut loss = LossReport::default();

    let id = body["id"]
        .as_str()
        .ok_or_else(|| CodecError::MissingField { field: "id".into() })?
        .to_string();

    let model = body["model"].as_str().unwrap_or(alias).to_string();

    let choices = body["choices"].as_array();
    let choice = &body["choices"][0];
    if choice.is_null() {
        return Err(CodecError::MissingField {
            field: "choices[0]".into(),
        });
    }
    // The IR carries a single assistant message; any additional choices (n > 1)
    // cannot be represented and are dropped.
    if let Some(arr) = choices {
        if arr.len() > 1 {
            loss.add(
                "choices",
                format!("{} choices", arr.len()),
                "choices[0] only",
                "IR represents a single assistant message; extra choices (n>1) dropped",
            );
        }
    }

    let msg = &choice["message"];
    let mut content: Vec<CanonicalContent> = Vec::new();

    // Text content (string or multimodal array of text parts)
    match &msg["content"] {
        Value::String(text) if !text.is_empty() => {
            content.push(CanonicalContent::Text {
                text: text.clone(),
            });
        }
        Value::Array(parts) => {
            for part in parts {
                if let Some(text) = part.as_str() {
                    if !text.is_empty() {
                        content.push(CanonicalContent::Text {
                            text: text.to_string(),
                        });
                    }
                    continue;
                }
                let ty = part.get("type").and_then(|t| t.as_str()).unwrap_or("text");
                if matches!(ty, "text" | "output_text") {
                    let text = part["text"].as_str().unwrap_or("").to_string();
                    if !text.is_empty() {
                        content.push(CanonicalContent::Text { text });
                    }
                }
            }
        }
        _ => {}
    }

    // Extension: reasoning_content (and common aliases) → Thinking.
    // Official OpenAI Chat does not document this field, but compatible
    // upstreams and multi-turn re-encode paths use it (CLIProxyAPI parity).
    for key in [
        "reasoning_content",
        "reasoning",
        "reasoning_text",
        "thinking",
    ] {
        if let Some(rc) = msg.get(key).and_then(|v| v.as_str()) {
            if !rc.is_empty() {
                content.push(CanonicalContent::Thinking {
                    thinking: rc.to_string(),
                    signature: Some("gpt#conduit".into()),
                });
                break;
            }
        }
    }

    // A refusal message has no IR representation; record it rather than drop it.
    if let Some(refusal) = msg["refusal"].as_str() {
        if !refusal.is_empty() {
            loss.add(
                "choices[0].message.refusal",
                refusal,
                "(dropped)",
                "OpenAI refusal message has no IR representation; skipped",
            );
        }
    }

    // Tool calls
    if let Some(tcs) = msg["tool_calls"].as_array() {
        for tc in tcs {
            let tc_id = tc["id"]
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
            let input: Value =
                serde_json::from_str(args_str).map_err(|e| CodecError::InvalidValue {
                    field: "tool_calls[].function.arguments".into(),
                    value: e.to_string(),
                })?;
            content.push(CanonicalContent::ToolUse {
                id: tc_id,
                name,
                input,
            });
        }
    }

    let finish_reason = decode_finish_reason(choice["finish_reason"].as_str());

    let usage = decode_usage(&body["usage"]);

    // Prefer the upstream unix `created` so re-encode stays a faithful proxy.
    let created_at = body
        .get("created")
        .and_then(|v| v.as_i64())
        .and_then(|secs| chrono::DateTime::from_timestamp(secs, 0))
        .unwrap_or_else(chrono::Utc::now);

    Ok((
        CanonicalChatResponse {
            id,
            request_id: String::new(), // filled in by the caller
            model,
            choices: vec![CanonicalMessage {
                role: Role::Assistant,
                content,
                name: None,
            }],
            finish_reason,
            usage,
            created_at,
        },
        loss,
    ))
}

pub(crate) fn decode_finish_reason(s: Option<&str>) -> FinishReason {
    match s {
        Some("stop") => FinishReason::Stop,
        Some("length") => FinishReason::Length,
        Some("tool_calls") => FinishReason::ToolCalls,
        Some("content_filter") => FinishReason::ContentFilter,
        Some(other) => FinishReason::Other(other.to_string()),
        None => FinishReason::Stop,
    }
}

pub(crate) fn decode_usage(usage: &Value) -> Usage {
    if usage.is_null()
        || usage.is_object() && usage.as_object().map(|m| m.is_empty()).unwrap_or(true)
    {
        return Usage::default();
    }

    let prompt_tokens = usage["prompt_tokens"].as_u64().unwrap_or(0) as u32;
    let completion_tokens = usage["completion_tokens"].as_u64().unwrap_or(0) as u32;
    let total_tokens = usage["total_tokens"]
        .as_u64()
        .unwrap_or((prompt_tokens + completion_tokens) as u64) as u32;

    // Cache tokens (prompt_tokens_details.*). Field names mirror Responses API's
    // input_tokens_details: cached_tokens = read, cache_write_tokens = write.
    // Older models only emit cached_tokens; write stays 0 when absent.
    let details = &usage["prompt_tokens_details"];
    let cache_read_tokens = details["cached_tokens"].as_u64().unwrap_or(0) as u32;
    let cache_write_tokens = details["cache_write_tokens"].as_u64().unwrap_or(0) as u32;

    // Reasoning tokens (completion_tokens_details.reasoning_tokens) — o1/o3
    let reasoning_tokens = usage["completion_tokens_details"]["reasoning_tokens"]
        .as_u64()
        .unwrap_or(0) as u32;

    Usage {
        prompt_tokens,
        completion_tokens,
        total_tokens,
        reasoning_tokens,
        cache_read_tokens,
        cache_write_tokens,
    }
}

/// Encode IR [`Usage`] as OpenAI Chat Completions `usage` (`CompletionUsage`).
///
/// Matches non-stream `encode_response` and stream `include_usage` final frames:
/// details objects are emitted only when the corresponding counts are non-zero
/// (official Chat samples; unlike Responses which always includes nested objects).
pub(crate) fn encode_chat_usage(usage: &Usage) -> Value {
    let mut out = serde_json::json!({
        "prompt_tokens": usage.prompt_tokens,
        "completion_tokens": usage.completion_tokens,
        "total_tokens": usage.total_tokens,
    });
    if usage.cache_read_tokens > 0 || usage.cache_write_tokens > 0 {
        let mut details = serde_json::Map::new();
        if usage.cache_read_tokens > 0 {
            details.insert(
                "cached_tokens".into(),
                serde_json::json!(usage.cache_read_tokens),
            );
        }
        if usage.cache_write_tokens > 0 {
            details.insert(
                "cache_write_tokens".into(),
                serde_json::json!(usage.cache_write_tokens),
            );
        }
        out["prompt_tokens_details"] = Value::Object(details);
    }
    if usage.reasoning_tokens > 0 {
        out["completion_tokens_details"] = serde_json::json!({
            "reasoning_tokens": usage.reasoning_tokens,
        });
    }
    out
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn basic_text_response() {
        let body = json!({
            "id": "chatcmpl-abc",
            "object": "chat.completion",
            "model": "gpt-4o",
            "choices": [{
                "index": 0,
                "message": {"role": "assistant", "content": "Hello!"},
                "finish_reason": "stop"
            }],
            "usage": {
                "prompt_tokens": 10,
                "completion_tokens": 5,
                "total_tokens": 15
            }
        });
        let resp = decode_response(body, "gpt-4o").unwrap().0;
        assert_eq!(resp.id, "chatcmpl-abc");
        assert_eq!(resp.finish_reason, FinishReason::Stop);
        assert_eq!(resp.usage.prompt_tokens, 10);
        assert_eq!(resp.usage.completion_tokens, 5);
        assert_eq!(resp.usage.total_tokens, 15);
        assert!(
            matches!(&resp.choices[0].content[0], CanonicalContent::Text { text } if text == "Hello!")
        );
    }

    #[test]
    fn tool_calls_decoded() {
        let body = json!({
            "id": "chatcmpl-xyz",
            "model": "gpt-4o",
            "choices": [{
                "index": 0,
                "message": {
                    "role": "assistant",
                    "content": null,
                    "tool_calls": [{
                        "id": "call_1",
                        "type": "function",
                        "function": {"name": "search", "arguments": "{\"q\":\"rust\"}"}
                    }]
                },
                "finish_reason": "tool_calls"
            }],
            "usage": {"prompt_tokens": 20, "completion_tokens": 10, "total_tokens": 30}
        });
        let resp = decode_response(body, "gpt-4o").unwrap().0;
        assert_eq!(resp.finish_reason, FinishReason::ToolCalls);
        if let CanonicalContent::ToolUse { id, name, input } = &resp.choices[0].content[0] {
            assert_eq!(id, "call_1");
            assert_eq!(name, "search");
            assert_eq!(input["q"].as_str().unwrap(), "rust");
        } else {
            panic!("Expected ToolUse");
        }
    }

    #[test]
    fn reasoning_tokens_mapped() {
        let body = json!({
            "id": "chatcmpl-o1",
            "model": "o1",
            "choices": [{
                "index": 0,
                "message": {"role": "assistant", "content": "42"},
                "finish_reason": "stop"
            }],
            "usage": {
                "prompt_tokens": 50,
                "completion_tokens": 200,
                "total_tokens": 250,
                "completion_tokens_details": {"reasoning_tokens": 150}
            }
        });
        let resp = decode_response(body, "o1").unwrap().0;
        assert_eq!(resp.usage.reasoning_tokens, 150);
    }

    #[test]
    fn cache_read_tokens_mapped() {
        let body = json!({
            "id": "chatcmpl-cache",
            "model": "gpt-4o",
            "choices": [{
                "index": 0,
                "message": {"role": "assistant", "content": "ok"},
                "finish_reason": "stop"
            }],
            "usage": {
                "prompt_tokens": 100,
                "completion_tokens": 10,
                "total_tokens": 110,
                "prompt_tokens_details": {"cached_tokens": 80}
            }
        });
        let resp = decode_response(body, "gpt-4o").unwrap().0;
        assert_eq!(resp.usage.cache_read_tokens, 80);
        assert_eq!(resp.usage.cache_write_tokens, 0);
    }

    #[test]
    fn cache_write_tokens_mapped() {
        let body = json!({
            "id": "chatcmpl-cache-write",
            "model": "gpt-5.6",
            "choices": [{
                "index": 0,
                "message": {"role": "assistant", "content": "ok"},
                "finish_reason": "stop"
            }],
            "usage": {
                "prompt_tokens": 200,
                "completion_tokens": 10,
                "total_tokens": 210,
                "prompt_tokens_details": {
                    "cached_tokens": 50,
                    "cache_write_tokens": 120
                }
            }
        });
        let resp = decode_response(body, "gpt-5.6").unwrap().0;
        assert_eq!(resp.usage.cache_read_tokens, 50);
        assert_eq!(resp.usage.cache_write_tokens, 120);
    }

    #[test]
    fn missing_id_returns_error() {
        let body = json!({
            "model": "gpt-4o",
            "choices": [{"index": 0, "message": {"role": "assistant", "content": "hi"}, "finish_reason": "stop"}],
            "usage": {}
        });
        assert!(matches!(
            decode_response(body, "gpt-4o"),
            Err(CodecError::MissingField { .. })
        ));
    }

    #[test]
    fn non_stream_reasoning_content_becomes_thinking() {
        let body = json!({
            "id": "chatcmpl-r",
            "model": "compat",
            "choices": [{
                "index": 0,
                "message": {
                    "role": "assistant",
                    "content": "answer",
                    "reasoning_content": "step by step"
                },
                "finish_reason": "stop"
            }],
            "usage": {"prompt_tokens": 1, "completion_tokens": 2, "total_tokens": 3}
        });
        let resp = decode_response(body, "compat").unwrap().0;
        assert!(
            resp.choices[0].content.iter().any(
                |c| matches!(c, CanonicalContent::Text { text } if text == "answer")
            )
        );
        assert!(resp.choices[0].content.iter().any(|c| matches!(
            c,
            CanonicalContent::Thinking { thinking, signature }
                if thinking == "step by step"
                    && signature.as_deref() == Some("gpt#conduit")
        )));
    }

    #[test]
    fn non_stream_reasoning_reencodes_via_openai_codec() {
        use crate::openai::OpenAICodec;
        use crate::WireCodec;

        let body = json!({
            "id": "chatcmpl-r2",
            "model": "m",
            "choices": [{
                "index": 0,
                "message": {
                    "role": "assistant",
                    "content": "ok",
                    "reasoning_content": "think"
                },
                "finish_reason": "stop"
            }],
            "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2}
        });
        let (resp, _) = OpenAICodec::decode_response(body, "m").unwrap();
        let wire = OpenAICodec::encode_response(&resp);
        assert_eq!(wire["choices"][0]["message"]["content"], "ok");
        assert_eq!(
            wire["choices"][0]["message"]["reasoning_content"],
            "think"
        );
    }

    #[test]
    fn created_timestamp_preserved_from_body() {
        let body = json!({
            "id": "chatcmpl-ts",
            "object": "chat.completion",
            "created": 1_700_000_000,
            "model": "gpt-4o",
            "choices": [{
                "index": 0,
                "message": {"role": "assistant", "content": "hi"},
                "finish_reason": "stop"
            }],
            "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2}
        });
        let resp = decode_response(body, "gpt-4o").unwrap().0;
        assert_eq!(resp.created_at.timestamp(), 1_700_000_000);
    }

    #[test]
    fn encode_chat_usage_matches_non_stream_shape() {
        let usage = Usage {
            prompt_tokens: 50,
            completion_tokens: 10,
            total_tokens: 60,
            reasoning_tokens: 3,
            cache_read_tokens: 20,
            cache_write_tokens: 5,
        };
        let wire = encode_chat_usage(&usage);
        assert_eq!(wire["prompt_tokens"], 50);
        assert_eq!(wire["prompt_tokens_details"]["cached_tokens"], 20);
        assert_eq!(wire["prompt_tokens_details"]["cache_write_tokens"], 5);
        assert_eq!(wire["completion_tokens_details"]["reasoning_tokens"], 3);

        let zero = encode_chat_usage(&Usage {
            prompt_tokens: 1,
            completion_tokens: 1,
            total_tokens: 2,
            ..Default::default()
        });
        assert!(zero.get("prompt_tokens_details").is_none());
        assert!(zero.get("completion_tokens_details").is_none());
    }
}
