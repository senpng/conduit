use conduit_ir::{
    canonical::{
        CanonicalChatResponse, CanonicalContent, CanonicalMessage, FinishReason, Role, Usage,
    },
    error::CodecError,
};
use serde_json::Value;

/// Decode an OpenAI `/v1/chat/completions` response body into the canonical IR.
pub fn decode_response(body: Value, alias: &str) -> Result<CanonicalChatResponse, CodecError> {
    let id = body["id"]
        .as_str()
        .ok_or_else(|| CodecError::MissingField { field: "id".into() })?
        .to_string();

    let model = body["model"].as_str().unwrap_or(alias).to_string();

    let choice = &body["choices"][0];
    if choice.is_null() {
        return Err(CodecError::MissingField {
            field: "choices[0]".into(),
        });
    }

    let msg = &choice["message"];
    let mut content: Vec<CanonicalContent> = Vec::new();

    // Text content
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

    let created_at = chrono::Utc::now();

    Ok(CanonicalChatResponse {
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
    })
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

    // Cache tokens (prompt_tokens_details.cached_tokens)
    let cache_read_tokens = usage["prompt_tokens_details"]["cached_tokens"]
        .as_u64()
        .unwrap_or(0) as u32;

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
        cache_write_tokens: 0,
    }
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
        let resp = decode_response(body, "gpt-4o").unwrap();
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
        let resp = decode_response(body, "gpt-4o").unwrap();
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
        let resp = decode_response(body, "o1").unwrap();
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
        let resp = decode_response(body, "gpt-4o").unwrap();
        assert_eq!(resp.usage.cache_read_tokens, 80);
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
}
