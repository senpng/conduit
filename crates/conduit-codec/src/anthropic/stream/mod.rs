pub mod blocks;
pub mod encoder;
pub mod state;

use conduit_ir::{
    canonical::{BlockDelta, BlockKind, CanonicalChunk},
    error::CodecError,
};
use serde_json::Value;
pub use encoder::{
    encode_message_start_frame, encode_message_stop_frame, finish_reason_to_anthropic,
    AnthropicStreamEncoder, GPT_THINKING_SIGNATURE,
};
pub use state::{decode_event, AnthropicStreamState};

/// Decode a single Anthropic SSE line pair.
pub fn decode_chunk_event(event: &str, data: &str) -> Result<Vec<CanonicalChunk>, CodecError> {
    if data.trim().is_empty() {
        return Ok(vec![]);
    }
    let val: Value = serde_json::from_str(data)?;
    let mut state = AnthropicStreamState::new();
    decode_event(&mut state, event, &val)
}

/// Stateless encode of a single IR chunk (no full lifecycle).
///
/// Prefer [`AnthropicStreamEncoder`] for Claude Code / client-facing streams.
pub fn encode_chunk(chunk: &CanonicalChunk, _resp_id: &str) -> Option<String> {
    use serde_json::json;

    // Tool use start
    if chunk.delta.is_none()
        && chunk.finish_reason.is_none()
        && (chunk.tool_use_id.is_some() || chunk.tool_name.is_some())
    {
        let block = json!({
            "type": "tool_use",
            "id": chunk.tool_use_id.as_deref().unwrap_or(""),
            "name": chunk.tool_name.as_deref().unwrap_or(""),
            "input": {}
        });
        let data = json!({
            "type": "content_block_start",
            "index": chunk.block_index,
            "content_block": block,
        });
        return Some(format!("event: content_block_start\ndata: {data}\n\n"));
    }

    // Block start (kind set, no delta, no finish)
    if let (Some(kind), None) = (&chunk.block_kind, &chunk.delta) {
        if chunk.finish_reason.is_none() {
            let block = match kind {
                BlockKind::Text => json!({"type": "text", "text": ""}),
                BlockKind::Thinking => json!({"type": "thinking", "thinking": ""}),
                BlockKind::ToolUse => json!({
                    "type": "tool_use",
                    "id": chunk.tool_use_id.as_deref().unwrap_or(""),
                    "name": chunk.tool_name.as_deref().unwrap_or(""),
                    "input": {}
                }),
                _ => json!({"type": "text", "text": ""}),
            };
            let data = json!({
                "type": "content_block_start",
                "index": chunk.block_index,
                "content_block": block,
            });
            return Some(format!("event: content_block_start\ndata: {data}\n\n"));
        }
    }

    // Block delta
    if let Some(delta) = &chunk.delta {
        let delta_val = match delta {
            BlockDelta::TextDelta { text } => json!({"type": "text_delta", "text": text}),
            BlockDelta::InputJsonDelta { partial_json } => {
                json!({"type": "input_json_delta", "partial_json": partial_json})
            }
            BlockDelta::ThinkingDelta { thinking } => {
                json!({"type": "thinking_delta", "thinking": thinking})
            }
            BlockDelta::SignatureDelta { signature } => {
                json!({"type": "signature_delta", "signature": signature})
            }
            _ => return None,
        };
        let data = json!({
            "type": "content_block_delta",
            "index": chunk.block_index,
            "delta": delta_val,
        });
        return Some(format!("event: content_block_delta\ndata: {data}\n\n"));
    }

    // Finish → message_delta
    if let Some(fr) = &chunk.finish_reason {
        let stop_str = finish_reason_to_anthropic(fr);
        let usage_val = chunk
            .usage
            .as_ref()
            .map(|u| {
                json!({
                    "input_tokens": u.prompt_tokens,
                    "output_tokens": u.completion_tokens,
                })
            })
            .unwrap_or(json!({"input_tokens": 0, "output_tokens": 0}));
        let data = json!({
            "type": "message_delta",
            "delta": {"stop_reason": stop_str, "stop_sequence": null},
            "usage": usage_val,
        });
        return Some(format!("event: message_delta\ndata: {data}\n\n"));
    }

    // content_block_stop (empty marker with block_index)
    if chunk.block_kind.is_none()
        && chunk.delta.is_none()
        && chunk.usage.is_none()
        && chunk.tool_use_id.is_none()
        && chunk.tool_name.is_none()
    {
        let data = json!({"type": "content_block_stop", "index": chunk.block_index});
        return Some(format!("event: content_block_stop\ndata: {data}\n\n"));
    }

    None
}

pub fn encode_message_start(resp_id: &str, model: &str, prompt_tokens: u32) -> String {
    encode_message_start_frame(resp_id, model, prompt_tokens)
}

pub fn encode_message_stop() -> &'static str {
    encode_message_stop_frame()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use conduit_ir::canonical::BlockDelta;

    use conduit_ir::canonical::FinishReason;

    use super::*;

    #[test]
    fn encode_text_delta_sse() {
        let chunk = CanonicalChunk {
            request_id: String::new(),
            index: 0,
            block_index: 0,
            block_kind: None,
            delta: Some(BlockDelta::TextDelta {
                text: "hello".into(),
            }),
            finish_reason: None,
            usage: None,
            tool_use_id: None,
            tool_name: None,
        };
        let sse = encode_chunk(&chunk, "resp_1").unwrap();
        assert!(sse.contains("content_block_delta"));
        assert!(sse.contains("text_delta"));
        assert!(sse.contains("hello"));
    }

    #[test]
    fn encode_tool_use_block_start() {
        let chunk = CanonicalChunk {
            request_id: String::new(),
            index: 0,
            block_index: 1,
            block_kind: Some(BlockKind::ToolUse),
            delta: None,
            finish_reason: None,
            usage: None,
            tool_use_id: Some("tu_1".into()),
            tool_name: Some("search".into()),
        };
        let sse = encode_chunk(&chunk, "resp_1").unwrap();
        assert!(sse.contains("content_block_start"));
        assert!(sse.contains("tool_use"));
        assert!(sse.contains("tu_1"));
    }

    #[test]
    fn encoder_end_to_end_openai_style_text() {
        let mut enc = AnthropicStreamEncoder::new("msg_x", "model");
        let delta = CanonicalChunk {
            request_id: String::new(),
            index: 0,
            block_index: 0,
            block_kind: Some(BlockKind::Text),
            delta: Some(BlockDelta::TextDelta {
                text: "partial".into(),
            }),
            finish_reason: None,
            usage: None,
            tool_use_id: None,
            tool_name: None,
        };
        let fin = CanonicalChunk {
            request_id: String::new(),
            index: 0,
            block_index: 0,
            block_kind: None,
            delta: None,
            finish_reason: Some(FinishReason::Stop),
            usage: None,
            tool_use_id: None,
            tool_name: None,
        };
        let joined: String = enc
            .push(&delta)
            .into_iter()
            .chain(enc.push(&fin))
            .collect();
        assert!(joined.contains("content_block_start"));
        assert!(joined.contains("content_block_stop"));
        assert!(joined.contains("message_stop"));
    }
}
