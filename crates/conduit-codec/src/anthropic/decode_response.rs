use conduit_ir::{
    canonical::{
        CanonicalChatResponse, CanonicalContent, CanonicalMessage, FinishReason, Role, Usage,
    },
    error::CodecError,
    loss::LossReport,
};
use serde_json::Value;

/// Decode an Anthropic Messages API response body into the canonical IR.
///
/// The returned [`LossReport`] records any response content that could not be
/// represented in the IR (e.g. unknown content block types that were skipped),
/// so the audit trail reflects what was dropped rather than silently losing it.
pub fn decode_response(
    body: Value,
    alias: &str,
) -> Result<(CanonicalChatResponse, LossReport), CodecError> {
    let id = body["id"]
        .as_str()
        .ok_or_else(|| CodecError::MissingField { field: "id".into() })?
        .to_string();

    let model = body["model"].as_str().unwrap_or(alias).to_string();

    let content_blocks = body["content"]
        .as_array()
        .ok_or_else(|| CodecError::MissingField {
            field: "content".into(),
        })?;

    let mut loss = LossReport::default();
    let content = decode_content_blocks(content_blocks, &mut loss)?;

    let finish_reason = decode_finish_reason(body["stop_reason"].as_str());

    let usage = decode_usage(&body["usage"]);

    Ok((
        CanonicalChatResponse {
            id,
            request_id: String::new(),
            model,
            choices: vec![CanonicalMessage {
                role: Role::Assistant,
                content,
                name: None,
            }],
            finish_reason,
            usage,
            created_at: chrono::Utc::now(),
        },
        loss,
    ))
}

fn decode_content_blocks(
    blocks: &[Value],
    loss: &mut LossReport,
) -> Result<Vec<CanonicalContent>, CodecError> {
    let mut content = Vec::new();
    for block in blocks {
        match block["type"].as_str() {
            Some("text") => {
                let text = block["text"]
                    .as_str()
                    .ok_or_else(|| CodecError::MissingField {
                        field: "content[].text".into(),
                    })?
                    .to_string();
                content.push(CanonicalContent::Text { text });
            }
            Some("tool_use") => {
                let id = block["id"]
                    .as_str()
                    .ok_or_else(|| CodecError::MissingField {
                        field: "content[].id".into(),
                    })?
                    .to_string();
                let name = block["name"]
                    .as_str()
                    .ok_or_else(|| CodecError::MissingField {
                        field: "content[].name".into(),
                    })?
                    .to_string();
                let input = block["input"].clone();
                content.push(CanonicalContent::ToolUse { id, name, input });
            }
            Some("thinking") => {
                let thinking = block["thinking"].as_str().unwrap_or("").to_string();
                let signature = block["signature"].as_str().map(String::from);
                content.push(CanonicalContent::Thinking {
                    thinking,
                    signature,
                });
            }
            Some(other) => {
                tracing::debug!(
                    block_type = other,
                    "Skipping unknown Anthropic content block"
                );
                loss.add(
                    "content[].type",
                    other,
                    "(dropped)",
                    "unknown Anthropic content block type has no IR representation; skipped",
                );
            }
            None => {
                loss.add(
                    "content[].type",
                    "(missing)",
                    "(dropped)",
                    "Anthropic content block without a `type` field; skipped",
                );
            }
        }
    }
    Ok(content)
}

pub(crate) fn decode_finish_reason(s: Option<&str>) -> FinishReason {
    match s {
        Some("end_turn") => FinishReason::Stop,
        Some("tool_use") => FinishReason::ToolCalls,
        Some("max_tokens") => FinishReason::Length,
        Some("stop_sequence") => FinishReason::Stop,
        Some(other) => FinishReason::Other(other.to_string()),
        None => FinishReason::Stop,
    }
}

pub(crate) fn decode_usage(usage: &Value) -> Usage {
    // input_tokens → prompt_tokens
    let prompt_tokens = usage["input_tokens"].as_u64().unwrap_or(0) as u32;
    // output_tokens → completion_tokens
    let completion_tokens = usage["output_tokens"].as_u64().unwrap_or(0) as u32;
    // cache_read_input_tokens → cache_read_tokens
    let cache_read_tokens = usage["cache_read_input_tokens"].as_u64().unwrap_or(0) as u32;
    // cache_creation_input_tokens → cache_write_tokens
    let cache_write_tokens = usage["cache_creation_input_tokens"].as_u64().unwrap_or(0) as u32;

    Usage {
        prompt_tokens,
        completion_tokens,
        total_tokens: prompt_tokens + completion_tokens,
        reasoning_tokens: 0,
        cache_read_tokens,
        cache_write_tokens,
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
            "id": "msg_abc",
            "type": "message",
            "model": "claude-3-5-sonnet",
            "stop_reason": "end_turn",
            "content": [{"type": "text", "text": "Hello!"}],
            "usage": {"input_tokens": 10, "output_tokens": 5}
        });
        let resp = decode_response(body, "claude-3-5-sonnet").unwrap().0;
        assert_eq!(resp.id, "msg_abc");
        assert_eq!(resp.finish_reason, FinishReason::Stop);
        assert_eq!(resp.usage.prompt_tokens, 10);
        assert_eq!(resp.usage.completion_tokens, 5);
        assert_eq!(resp.usage.total_tokens, 15);
    }

    #[test]
    fn tool_use_response() {
        let body = json!({
            "id": "msg_tool",
            "model": "claude-3-5-sonnet",
            "stop_reason": "tool_use",
            "content": [{"type": "tool_use", "id": "tu_1", "name": "search", "input": {"q": "rust"}}],
            "usage": {"input_tokens": 20, "output_tokens": 15}
        });
        let resp = decode_response(body, "claude-3-5-sonnet").unwrap().0;
        assert_eq!(resp.finish_reason, FinishReason::ToolCalls);
        if let CanonicalContent::ToolUse { id, name, .. } = &resp.choices[0].content[0] {
            assert_eq!(id, "tu_1");
            assert_eq!(name, "search");
        } else {
            panic!("expected ToolUse");
        }
    }

    #[test]
    fn cache_tokens_mapped() {
        let body = json!({
            "id": "msg_cache",
            "model": "claude-3-5-sonnet",
            "stop_reason": "end_turn",
            "content": [{"type": "text", "text": "ok"}],
            "usage": {
                "input_tokens": 100,
                "output_tokens": 10,
                "cache_read_input_tokens": 80,
                "cache_creation_input_tokens": 20
            }
        });
        let resp = decode_response(body, "claude-3-5-sonnet").unwrap().0;
        assert_eq!(resp.usage.cache_read_tokens, 80);
        assert_eq!(resp.usage.cache_write_tokens, 20);
    }

    #[test]
    fn thinking_block_decoded() {
        let body = json!({
            "id": "msg_think",
            "model": "claude-3-7-sonnet",
            "stop_reason": "end_turn",
            "content": [
                {"type": "thinking", "thinking": "Let me reason...", "signature": "sig123"},
                {"type": "text", "text": "The answer is 42."}
            ],
            "usage": {"input_tokens": 5, "output_tokens": 10}
        });
        let resp = decode_response(body, "claude-3-7-sonnet").unwrap().0;
        assert_eq!(resp.choices[0].content.len(), 2);
        if let CanonicalContent::Thinking {
            thinking,
            signature,
        } = &resp.choices[0].content[0]
        {
            assert_eq!(thinking, "Let me reason...");
            assert_eq!(signature.as_deref(), Some("sig123"));
        } else {
            panic!("expected Thinking");
        }
    }

    #[test]
    fn unknown_content_block_recorded_in_loss() {
        let body = json!({
            "id": "msg_unknown",
            "model": "claude-3-5-sonnet",
            "stop_reason": "end_turn",
            "content": [
                {"type": "text", "text": "hi"},
                {"type": "redacted_thinking", "data": "opaque"}
            ],
            "usage": {"input_tokens": 1, "output_tokens": 1}
        });
        let (resp, loss) = decode_response(body, "claude-3-5-sonnet").unwrap();
        // Known text block is kept; the unknown block is dropped but recorded.
        assert_eq!(resp.choices[0].content.len(), 1);
        assert_eq!(loss.len(), 1);
        assert_eq!(loss.warnings[0].field, "content[].type");
        assert_eq!(loss.warnings[0].original, "redacted_thinking");
    }
}
