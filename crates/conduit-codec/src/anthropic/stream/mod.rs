pub mod blocks;
pub mod encoder;
pub mod state;

use conduit_ir::{
    canonical::{BlockDelta, BlockKind, CanonicalChunk},
    error::CodecError,
};
pub use encoder::{
    anthropic_usage_json, encode_message_start_frame, encode_message_start_simple,
    encode_message_stop_frame, finish_reason_to_anthropic, AnthropicStreamEncoder,
    GPT_THINKING_SIGNATURE,
};
use encoder::{
    content_block_stop, input_json_delta, signature_delta, sse_event, text_block_start, text_delta,
    thinking_block_start, thinking_delta, tool_use_start,
};
use serde_json::{json, Value};
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
    let idx = chunk.block_index;

    // Tool use start (id/name present, no delta/finish) — before bare kind match
    // so ToolUse kind with ids reuses the same frame builder.
    if chunk.delta.is_none()
        && chunk.finish_reason.is_none()
        && (chunk.tool_use_id.is_some() || chunk.tool_name.is_some())
    {
        return Some(tool_use_start(
            idx,
            chunk.tool_use_id.as_deref().unwrap_or(""),
            chunk.tool_name.as_deref().unwrap_or(""),
        ));
    }

    // Bare block start (kind set, no delta, no finish)
    if let (Some(kind), None) = (&chunk.block_kind, &chunk.delta) {
        if chunk.finish_reason.is_none() {
            return Some(match kind {
                BlockKind::Thinking => thinking_block_start(idx),
                BlockKind::ToolUse => tool_use_start(
                    idx,
                    chunk.tool_use_id.as_deref().unwrap_or(""),
                    chunk.tool_name.as_deref().unwrap_or(""),
                ),
                // Text and unknown kinds → empty text block.
                _ => text_block_start(idx),
            });
        }
    }

    // Block delta
    if let Some(delta) = &chunk.delta {
        return match delta {
            BlockDelta::TextDelta { text } => Some(text_delta(idx, text)),
            BlockDelta::InputJsonDelta { partial_json } => {
                Some(input_json_delta(idx, partial_json))
            }
            BlockDelta::ThinkingDelta { thinking } => Some(thinking_delta(idx, thinking)),
            BlockDelta::SignatureDelta { signature } => Some(signature_delta(idx, signature)),
            _ => None,
        };
    }

    // Finish → message_delta
    if let Some(fr) = &chunk.finish_reason {
        let usage = chunk
            .usage
            .clone()
            .unwrap_or_default();
        let data = json!({
            "type": "message_delta",
            "delta": {
                "stop_reason": finish_reason_to_anthropic(fr),
                "stop_sequence": null,
            },
            "usage": anthropic_usage_json(&usage),
        });
        return Some(sse_event("message_delta", &data));
    }

    // content_block_stop (empty marker with block_index)
    if chunk.block_kind.is_none()
        && chunk.delta.is_none()
        && chunk.usage.is_none()
        && chunk.tool_use_id.is_none()
        && chunk.tool_name.is_none()
    {
        return Some(content_block_stop(idx));
    }

    None
}

pub fn encode_message_start(resp_id: &str, model: &str, prompt_tokens: u32) -> String {
    encode_message_start_simple(resp_id, model, prompt_tokens)
}

pub fn encode_message_stop() -> &'static str {
    encode_message_stop_frame()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use conduit_ir::canonical::{BlockDelta, FinishReason};

    use super::*;

    #[test]
    fn encode_text_delta_sse() {
        let chunk = CanonicalChunk {
            delta: Some(BlockDelta::TextDelta {
                text: "hello".into(),
            }),
            ..Default::default()
        };
        let sse = encode_chunk(&chunk, "resp_1").unwrap();
        assert!(sse.contains("content_block_delta"));
        assert!(sse.contains("text_delta"));
        assert!(sse.contains("hello"));
    }

    #[test]
    fn encode_tool_use_block_start() {
        let chunk = CanonicalChunk {
            block_index: 1,
            block_kind: Some(BlockKind::ToolUse),
            tool_use_id: Some("tu_1".into()),
            tool_name: Some("search".into()),
            ..Default::default()
        };
        let sse = encode_chunk(&chunk, "resp_1").unwrap();
        assert!(sse.contains("content_block_start"));
        assert!(sse.contains("tool_use"));
        assert!(sse.contains("tu_1"));
    }

    #[test]
    fn encoder_end_to_end_openai_style_text() {
        let mut enc = AnthropicStreamEncoder::new("msg_x", "model");
        let delta = CanonicalChunk::text_delta("partial");
        let fin = CanonicalChunk::finish(FinishReason::Stop, None);
        let joined: String = enc.push(&delta).into_iter().chain(enc.push(&fin)).collect();
        assert!(joined.contains("content_block_start"));
        assert!(joined.contains("content_block_stop"));
        assert!(joined.contains("message_stop"));
    }
}
