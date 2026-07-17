use conduit_ir::{
    canonical::{BlockDelta, BlockKind, CanonicalChunk},
    error::CodecError,
};
use serde_json::Value;

use super::blocks::{BlockState, StreamUsage};
use crate::anthropic::decode_response::{decode_finish_reason, decode_usage};

/// Stateful decoder for the Anthropic streaming SSE protocol.
///
/// Call `process_event` for each `event: <type>` / `data: <json>` pair.
/// Outputs zero or more `CanonicalChunk`s per call.
#[derive(Debug, Default)]
pub struct AnthropicStreamState {
    /// Open blocks indexed by their Anthropic block index.
    open_blocks: std::collections::HashMap<u32, BlockState>,
    /// Usage accumulated from message_start and message_delta.
    usage: StreamUsage,
    /// Model string from message_start.
    model: String,
}

impl AnthropicStreamState {
    pub fn new() -> Self {
        Self::default()
    }

    /// Process a single event and return any canonical chunks it produces.
    pub fn process_event(
        &mut self,
        event: &str,
        data: &Value,
    ) -> Result<Vec<CanonicalChunk>, CodecError> {
        match event {
            "message_start" => self.on_message_start(data),
            "content_block_start" => self.on_content_block_start(data),
            "content_block_delta" => self.on_content_block_delta(data),
            "content_block_stop" => self.on_content_block_stop(data),
            "message_delta" => self.on_message_delta(data),
            "message_stop" => Ok(vec![make_stop_chunk()]),
            "ping" => Ok(vec![]),
            _ => Ok(vec![]),
        }
    }

    // -----------------------------------------------------------------------

    fn on_message_start(&mut self, data: &Value) -> Result<Vec<CanonicalChunk>, CodecError> {
        let msg = &data["message"];
        self.model = msg["model"].as_str().unwrap_or("").to_string();

        // Seed usage from message_start (input_tokens arrive here).
        let u = decode_usage(&msg["usage"]);
        self.usage.prompt_tokens += u.prompt_tokens;
        self.usage.cache_read_tokens += u.cache_read_tokens;
        self.usage.cache_write_tokens += u.cache_write_tokens;

        Ok(vec![CanonicalChunk::default()])
    }

    fn on_content_block_start(&mut self, data: &Value) -> Result<Vec<CanonicalChunk>, CodecError> {
        let index = data["index"].as_u64().unwrap_or(0) as u32;
        let block = &data["content_block"];
        let block_type = block["type"].as_str().unwrap_or("text");

        let state = match block_type {
            "text" => BlockState::new_text(index),
            "thinking" => BlockState::new_thinking(index),
            "tool_use" => {
                let id = block["id"].as_str().unwrap_or("").to_string();
                let name = block["name"].as_str().unwrap_or("").to_string();
                BlockState::new_tool_use(index, id.clone(), name.clone())
            }
            _ => BlockState::new_text(index),
        };

        let (tool_use_id, tool_name, kind) = match block_type {
            "tool_use" => {
                let id = block["id"].as_str().unwrap_or("").to_string();
                let name = block["name"].as_str().unwrap_or("").to_string();
                (Some(id), Some(name), BlockKind::ToolUse)
            }
            "thinking" => (None, None, BlockKind::Thinking),
            _ => (None, None, BlockKind::Text),
        };

        self.open_blocks.insert(index, state);

        Ok(vec![CanonicalChunk {
            block_index: index,
            block_kind: Some(kind),
            tool_use_id,
            tool_name,
            ..Default::default()
        }])
    }

    fn on_content_block_delta(&mut self, data: &Value) -> Result<Vec<CanonicalChunk>, CodecError> {
        let index = data["index"].as_u64().unwrap_or(0) as u32;
        let delta_val = &data["delta"];
        let delta_type = delta_val["type"].as_str().unwrap_or("");

        let block_delta = match delta_type {
            "text_delta" => {
                let text = delta_val["text"].as_str().unwrap_or("").to_string();
                if let Some(bs) = self.open_blocks.get_mut(&index) {
                    bs.text_buf.push_str(&text);
                }
                BlockDelta::TextDelta { text }
            }
            "input_json_delta" => {
                let partial = delta_val["partial_json"].as_str().unwrap_or("").to_string();
                if let Some(bs) = self.open_blocks.get_mut(&index) {
                    bs.json_buf.push_str(&partial);
                }
                BlockDelta::InputJsonDelta {
                    partial_json: partial,
                }
            }
            "thinking_delta" => {
                let thinking = delta_val["thinking"].as_str().unwrap_or("").to_string();
                if let Some(bs) = self.open_blocks.get_mut(&index) {
                    bs.text_buf.push_str(&thinking);
                }
                BlockDelta::ThinkingDelta { thinking }
            }
            "signature_delta" => {
                let sig = delta_val["signature"].as_str().unwrap_or("").to_string();
                if let Some(bs) = self.open_blocks.get_mut(&index) {
                    bs.signature = Some(sig.clone());
                }
                BlockDelta::SignatureDelta { signature: sig }
            }
            _ => return Ok(vec![]),
        };

        Ok(vec![CanonicalChunk {
            block_index: index,
            delta: Some(block_delta),
            ..Default::default()
        }])
    }

    fn on_content_block_stop(&mut self, data: &Value) -> Result<Vec<CanonicalChunk>, CodecError> {
        let index = data["index"].as_u64().unwrap_or(0) as u32;
        self.open_blocks.remove(&index);

        Ok(vec![CanonicalChunk {
            block_index: index,
            ..Default::default()
        }])
    }

    fn on_message_delta(&mut self, data: &Value) -> Result<Vec<CanonicalChunk>, CodecError> {
        let stop_reason = decode_finish_reason(data["delta"]["stop_reason"].as_str());

        // Accumulate output usage from message_delta.
        if let Some(u_val) = data.get("usage") {
            let u = decode_usage(u_val);
            self.usage.completion_tokens += u.completion_tokens;
            // message_delta may also contain cache tokens for output
            self.usage.cache_read_tokens += u.cache_read_tokens;
            self.usage.cache_write_tokens += u.cache_write_tokens;
        }

        let usage: conduit_ir::canonical::Usage = self.usage.clone().into();

        Ok(vec![CanonicalChunk::finish(stop_reason, Some(usage))])
    }
}

fn make_stop_chunk() -> CanonicalChunk {
    CanonicalChunk::default()
}

/// Decode a single Anthropic SSE event into canonical chunks.
///
/// `event` is the SSE `event:` field; `data` is the parsed JSON from `data:`.
pub fn decode_event(
    state: &mut AnthropicStreamState,
    event: &str,
    data: &Value,
) -> Result<Vec<CanonicalChunk>, CodecError> {
    state.process_event(event, data)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use conduit_ir::canonical::FinishReason;
    use serde_json::json;

    use super::*;

    fn make_state() -> AnthropicStreamState {
        AnthropicStreamState::new()
    }

    #[test]
    fn message_start_emits_chunk() {
        let mut s = make_state();
        let data = json!({
            "type": "message_start",
            "message": {
                "id": "msg_1", "model": "claude-3-5-sonnet", "role": "assistant",
                "content": [], "stop_reason": null,
                "usage": {"input_tokens": 25, "output_tokens": 0}
            }
        });
        let chunks = s.process_event("message_start", &data).unwrap();
        assert_eq!(chunks.len(), 1);
        assert_eq!(s.usage.prompt_tokens, 25);
    }

    #[test]
    fn text_block_lifecycle() {
        let mut s = make_state();

        let start = json!({"type": "content_block_start", "index": 0, "content_block": {"type": "text", "text": ""}});
        let chunks = s.process_event("content_block_start", &start).unwrap();
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].block_kind, Some(BlockKind::Text));

        let delta = json!({"type": "content_block_delta", "index": 0, "delta": {"type": "text_delta", "text": "Hello"}});
        let chunks = s.process_event("content_block_delta", &delta).unwrap();
        assert_eq!(chunks.len(), 1);
        assert!(
            matches!(&chunks[0].delta, Some(BlockDelta::TextDelta { text }) if text == "Hello")
        );

        let stop = json!({"type": "content_block_stop", "index": 0});
        let chunks = s.process_event("content_block_stop", &stop).unwrap();
        assert_eq!(chunks.len(), 1);
        assert!(s.open_blocks.is_empty());
    }

    #[test]
    fn tool_use_block_lifecycle() {
        let mut s = make_state();

        let start = json!({
            "type": "content_block_start", "index": 1,
            "content_block": {"type": "tool_use", "id": "tu_1", "name": "search", "input": {}}
        });
        let chunks = s.process_event("content_block_start", &start).unwrap();
        assert_eq!(chunks[0].tool_use_id.as_deref(), Some("tu_1"));
        assert_eq!(chunks[0].tool_name.as_deref(), Some("search"));

        let delta = json!({
            "type": "content_block_delta", "index": 1,
            "delta": {"type": "input_json_delta", "partial_json": "{\"q\":"}
        });
        let chunks = s.process_event("content_block_delta", &delta).unwrap();
        assert!(
            matches!(&chunks[0].delta, Some(BlockDelta::InputJsonDelta { partial_json }) if partial_json == "{\"q\":")
        );
    }

    #[test]
    fn message_delta_carries_finish_reason_and_usage() {
        let mut s = make_state();
        s.usage.prompt_tokens = 10;

        let data = json!({
            "type": "message_delta",
            "delta": {"stop_reason": "tool_use", "stop_sequence": null},
            "usage": {"output_tokens": 42, "cache_creation_input_tokens": 5, "cache_read_input_tokens": 8}
        });
        let chunks = s.process_event("message_delta", &data).unwrap();
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].finish_reason, Some(FinishReason::ToolCalls));
        let u = chunks[0].usage.as_ref().unwrap();
        assert_eq!(u.completion_tokens, 42);
        assert_eq!(u.prompt_tokens, 10);
        assert_eq!(u.cache_write_tokens, 5);
        assert_eq!(u.cache_read_tokens, 8);
    }

    #[test]
    fn message_stop_emits_chunk() {
        let mut s = make_state();
        let chunks = s
            .process_event("message_stop", &json!({"type": "message_stop"}))
            .unwrap();
        assert_eq!(chunks.len(), 1);
    }

    #[test]
    fn ping_emits_nothing() {
        let mut s = make_state();
        let chunks = s.process_event("ping", &json!({})).unwrap();
        assert!(chunks.is_empty());
    }
}
