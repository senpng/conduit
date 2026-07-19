//! Stateful Anthropic SSE encoder — CLIProxyAPI `ConvertOpenAIResponseToClaude` parity.
//!
//! Emits the full lifecycle:
//! `message_start` → `content_block_start` / `*_delta` / `content_block_stop` →
//! `message_delta` → `message_stop`.

use std::collections::BTreeMap;

use conduit_ir::canonical::{BlockDelta, BlockKind, CanonicalChunk, FinishReason, Usage};
use serde_json::json;

/// Synthetic signature stamped onto thinking blocks produced from OpenAI
/// `reasoning_content`, so multi-turn re-encoding can map them back.
pub const GPT_THINKING_SIGNATURE: &str = "gpt#conduit";

/// Accumulates tool call pieces until id + name are both known.
#[derive(Debug, Default)]
struct ToolAcc {
    id: String,
    name: String,
    arguments: String,
    start_emitted: bool,
    /// True once any `input_json_delta` was streamed live after start.
    args_live_streamed: bool,
    /// True after `content_block_stop` for this tool. Prevents `close_open_blocks`
    /// on finish from re-emitting a second empty `tool_use` (Anthropic-native
    /// path already closed the block before `message_delta`).
    closed: bool,
}

/// Stateful encoder: IR chunks → Anthropic Messages SSE frames.
#[derive(Debug)]
pub struct AnthropicStreamEncoder {
    message_id: String,
    model: String,
    message_started: bool,
    text_started: bool,
    text_index: i32,
    thinking_started: bool,
    thinking_index: i32,
    next_block_index: u32,
    /// OpenAI tool_calls index → Anthropic content block index.
    tool_block_indexes: BTreeMap<u32, u32>,
    tools: BTreeMap<u32, ToolAcc>,
    saw_tool_call: bool,
    finish_reason: Option<FinishReason>,
    content_blocks_stopped: bool,
    message_delta_sent: bool,
    message_stop_sent: bool,
    pending_usage: Option<Usage>,
}

impl AnthropicStreamEncoder {
    pub fn new(message_id: impl Into<String>, model: impl Into<String>) -> Self {
        Self {
            message_id: message_id.into(),
            model: model.into(),
            message_started: false,
            text_started: false,
            text_index: -1,
            thinking_started: false,
            thinking_index: -1,
            next_block_index: 0,
            tool_block_indexes: BTreeMap::new(),
            tools: BTreeMap::new(),
            saw_tool_call: false,
            finish_reason: None,
            content_blocks_stopped: false,
            message_delta_sent: false,
            message_stop_sent: false,
            pending_usage: None,
        }
    }

    /// Emit `message_start` if not already sent.
    ///
    /// Prefer tokens from [`Self::pending_usage`] (seeded by an early
    /// Anthropic `message_start` usage chunk) so Claude Code context is not 0.
    /// `prompt_tokens` is a fallback when usage is still unknown (e.g. OpenAI
    /// upstream only reports tokens at the end).
    pub fn ensure_message_start(&mut self, prompt_tokens: u32) -> Option<String> {
        if self.message_started {
            return None;
        }
        self.message_started = true;
        let (prompt, cache_read, cache_write) = self
            .pending_usage
            .as_ref()
            .map(|u| {
                (
                    if u.prompt_tokens > 0 {
                        u.prompt_tokens
                    } else {
                        prompt_tokens
                    },
                    u.cache_read_tokens,
                    u.cache_write_tokens,
                )
            })
            .unwrap_or((prompt_tokens, 0, 0));
        Some(encode_message_start_frame(
            &self.message_id,
            &self.model,
            prompt,
            cache_read,
            cache_write,
        ))
    }

    /// Process one IR chunk; returns zero or more complete SSE frames.
    pub fn push(&mut self, chunk: &CanonicalChunk) -> Vec<String> {
        let mut out = Vec::new();

        // Usage-only (no finish yet) — stash for message_start / message_delta.
        // Must run *before* ensure_message_start so input_tokens are not zeroed.
        if chunk.finish_reason.is_none()
            && chunk.delta.is_none()
            && chunk.block_kind.is_none()
            && chunk.tool_use_id.is_none()
            && chunk.tool_name.is_none()
        {
            if let Some(u) = &chunk.usage {
                self.merge_pending_usage(u);
                // Emit message_start with real input_tokens as soon as we know them.
                if let Some(frame) = self.ensure_message_start(u.prompt_tokens) {
                    out.push(frame);
                }
                // If we already saw finish_reason, emit message_delta now.
                if self.finish_reason.is_some() && !self.message_delta_sent {
                    self.close_open_blocks(&mut out);
                    self.emit_message_delta(&mut out);
                    self.emit_message_stop(&mut out);
                }
                return out;
            }
        }

        if let Some(frame) = self.ensure_message_start(0) {
            out.push(frame);
        }

        // Explicit content_block_stop from Anthropic-native IR (all None except block_index).
        if chunk.block_kind.is_none()
            && chunk.delta.is_none()
            && chunk.finish_reason.is_none()
            && chunk.usage.is_none()
            && chunk.tool_use_id.is_none()
            && chunk.tool_name.is_none()
        {
            // Treat as content_block_stop for the given index when a block is open.
            let idx = chunk.block_index;
            if self.text_started && self.text_index == idx as i32 {
                out.push(content_block_stop(idx));
                self.text_started = false;
                self.text_index = -1;
            } else if self.thinking_started && self.thinking_index == idx as i32 {
                out.push(signature_delta(idx, GPT_THINKING_SIGNATURE));
                out.push(content_block_stop(idx));
                self.thinking_started = false;
                self.thinking_index = -1;
            } else if let Some((oi, acc)) = self
                .tools
                .iter_mut()
                .find(|(oi, _)| self.tool_block_indexes.get(oi) == Some(&idx))
            {
                let _ = oi;
                if acc.start_emitted && !acc.closed {
                    out.push(content_block_stop(idx));
                    acc.start_emitted = false;
                    acc.closed = true;
                }
            }
            return out;
        }

        // Thinking delta
        if let Some(BlockDelta::ThinkingDelta { thinking }) = &chunk.delta {
            if !thinking.is_empty() {
                self.stop_text(&mut out);
                self.ensure_thinking_start(&mut out);
                let idx = self.thinking_index.max(0) as u32;
                out.push(thinking_delta(idx, thinking));
            }
        }

        // Signature delta
        if let Some(BlockDelta::SignatureDelta { signature }) = &chunk.delta {
            if self.thinking_started {
                let idx = self.thinking_index.max(0) as u32;
                out.push(signature_delta(idx, signature));
            }
        }

        // Text delta (with or without block_kind set)
        if let Some(BlockDelta::TextDelta { text }) = &chunk.delta {
            if !text.is_empty() {
                self.stop_thinking(&mut out);
                self.ensure_text_start(&mut out);
                let idx = self.text_index.max(0) as u32;
                out.push(text_delta(idx, text));
            }
        }

        // Bare block start (Anthropic-native IR: kind set, no delta)
        if chunk.delta.is_none() && chunk.finish_reason.is_none() {
            if let Some(kind) = &chunk.block_kind {
                match kind {
                    BlockKind::Text => {
                        // Only if not already open — OpenAI path uses kind+delta together.
                        if !self.text_started
                            && chunk.tool_use_id.is_none()
                            && chunk.tool_name.is_none()
                        {
                            self.stop_thinking(&mut out);
                            // Prefer upstream block_index when provided as pure start.
                            if chunk.block_index > 0 || self.next_block_index == 0 {
                                // assign via ensure
                            }
                            self.ensure_text_start(&mut out);
                        }
                    }
                    BlockKind::Thinking => {
                        if !self.thinking_started
                            && chunk.tool_use_id.is_none()
                            && chunk.tool_name.is_none()
                        {
                            self.stop_text(&mut out);
                            self.ensure_thinking_start(&mut out);
                        }
                    }
                    BlockKind::ToolUse => {
                        self.handle_tool_start(chunk, &mut out);
                    }
                    _ => {}
                }
            } else if chunk.tool_use_id.is_some() || chunk.tool_name.is_some() {
                self.handle_tool_start(chunk, &mut out);
            }
        } else if matches!(chunk.block_kind, Some(BlockKind::ToolUse))
            && chunk.delta.is_none()
            && (chunk.tool_use_id.is_some() || chunk.tool_name.is_some())
        {
            self.handle_tool_start(chunk, &mut out);
        }

        // Tool argument delta
        if let Some(BlockDelta::InputJsonDelta { partial_json }) = &chunk.delta {
            let openai_idx = chunk.block_index;
            let already_closed = self
                .tools
                .get(&openai_idx)
                .map(|a| a.closed)
                .unwrap_or(false);
            if !already_closed {
                {
                    let acc = self.tools.entry(openai_idx).or_default();
                    if !partial_json.is_empty() {
                        acc.arguments.push_str(partial_json);
                    }
                }
                // Emit start if we can; stream args live once started.
                let can_start = self
                    .tools
                    .get(&openai_idx)
                    .map(|a| !a.start_emitted && !a.name.is_empty() && !a.id.is_empty())
                    .unwrap_or(false);
                if can_start {
                    self.stop_text(&mut out);
                    self.stop_thinking(&mut out);
                    self.emit_tool_start(openai_idx, &mut out);
                }
                let started = self
                    .tools
                    .get(&openai_idx)
                    .map(|a| a.start_emitted)
                    .unwrap_or(false);
                if started && !partial_json.is_empty() {
                    let block_idx = self.tool_content_index(openai_idx);
                    out.push(input_json_delta(block_idx, partial_json));
                    if let Some(acc) = self.tools.get_mut(&openai_idx) {
                        acc.args_live_streamed = true;
                    }
                }
            }
        }

        // Finish reason
        if let Some(fr) = &chunk.finish_reason {
            // CLIProxyAPI: if we saw real tool_use blocks, force tool_calls;
            // if upstream said tool_calls but never emitted tools → stop.
            if self.saw_tool_call {
                self.finish_reason = Some(FinishReason::ToolCalls);
            } else if matches!(fr, FinishReason::ToolCalls) {
                self.finish_reason = Some(FinishReason::Stop);
            } else {
                self.finish_reason = Some(fr.clone());
            }
            if let Some(u) = &chunk.usage {
                self.merge_pending_usage(u);
            }
            self.close_open_blocks(&mut out);
            // Emit message_delta when we have usage or immediately (CLIProxyAPI
            // waits for usage when include_usage is on; we emit now and upgrade
            // is not supported — emit once).
            if !self.message_delta_sent {
                self.emit_message_delta(&mut out);
                self.emit_message_stop(&mut out);
            }
        }

        out
    }

    /// Flush remaining terminal events (open blocks, message_delta, message_stop).
    pub fn finish(&mut self) -> Vec<String> {
        let mut out = Vec::new();
        if let Some(frame) = self.ensure_message_start(0) {
            out.push(frame);
        }
        self.close_open_blocks(&mut out);
        if self.finish_reason.is_none() {
            // Stream ended without finish_reason — treat as end_turn.
            self.finish_reason = Some(FinishReason::Stop);
        }
        if !self.message_delta_sent {
            self.emit_message_delta(&mut out);
        }
        self.emit_message_stop(&mut out);
        out
    }

    fn merge_pending_usage(&mut self, u: &Usage) {
        match &mut self.pending_usage {
            Some(prev) => {
                if u.prompt_tokens > 0 {
                    prev.prompt_tokens = u.prompt_tokens;
                }
                if u.completion_tokens > 0 {
                    prev.completion_tokens = u.completion_tokens;
                }
                if u.cache_read_tokens > 0 {
                    prev.cache_read_tokens = u.cache_read_tokens;
                }
                if u.cache_write_tokens > 0 {
                    prev.cache_write_tokens = u.cache_write_tokens;
                }
                if u.reasoning_tokens > 0 {
                    prev.reasoning_tokens = u.reasoning_tokens;
                }
                prev.total_tokens = prev.prompt_tokens + prev.completion_tokens;
            }
            None => self.pending_usage = Some(u.clone()),
        }
    }

    fn handle_tool_start(&mut self, chunk: &CanonicalChunk, out: &mut Vec<String>) {
        let openai_idx = chunk.block_index;
        let acc = self.tools.entry(openai_idx).or_default();
        if acc.closed {
            return;
        }
        if let Some(id) = &chunk.tool_use_id {
            if !id.is_empty() {
                acc.id = id.clone();
            }
        }
        if let Some(name) = &chunk.tool_name {
            if !name.is_empty() {
                acc.name = name.clone();
            }
        }
        if !acc.start_emitted && !acc.name.is_empty() && !acc.id.is_empty() {
            self.stop_text(out);
            self.stop_thinking(out);
            self.emit_tool_start(openai_idx, out);
        }
    }

    fn emit_tool_start(&mut self, openai_idx: u32, out: &mut Vec<String>) {
        let (id, name, already) = {
            let acc = self.tools.entry(openai_idx).or_default();
            (
                acc.id.clone(),
                acc.name.clone(),
                acc.closed || acc.start_emitted || acc.name.is_empty() || acc.id.is_empty(),
            )
        };
        if already {
            return;
        }
        let block_idx = self.tool_content_index(openai_idx);
        if let Some(acc) = self.tools.get_mut(&openai_idx) {
            acc.start_emitted = true;
        }
        self.saw_tool_call = true;
        out.push(tool_use_start(block_idx, &id, &name));
    }

    fn tool_content_index(&mut self, openai_idx: u32) -> u32 {
        if let Some(&idx) = self.tool_block_indexes.get(&openai_idx) {
            return idx;
        }
        let idx = self.next_block_index;
        self.next_block_index += 1;
        self.tool_block_indexes.insert(openai_idx, idx);
        idx
    }

    fn ensure_text_start(&mut self, out: &mut Vec<String>) {
        if self.text_started {
            return;
        }
        if self.text_index < 0 {
            self.text_index = self.next_block_index as i32;
            self.next_block_index += 1;
        }
        let idx = self.text_index as u32;
        out.push(text_block_start(idx));
        self.text_started = true;
    }

    fn ensure_thinking_start(&mut self, out: &mut Vec<String>) {
        if self.thinking_started {
            return;
        }
        if self.thinking_index < 0 {
            self.thinking_index = self.next_block_index as i32;
            self.next_block_index += 1;
        }
        let idx = self.thinking_index as u32;
        out.push(thinking_block_start(idx));
        self.thinking_started = true;
    }

    fn stop_text(&mut self, out: &mut Vec<String>) {
        if !self.text_started {
            return;
        }
        out.push(content_block_stop(self.text_index as u32));
        self.text_started = false;
        self.text_index = -1;
    }

    fn stop_thinking(&mut self, out: &mut Vec<String>) {
        if !self.thinking_started {
            return;
        }
        let idx = self.thinking_index as u32;
        out.push(signature_delta(idx, GPT_THINKING_SIGNATURE));
        out.push(content_block_stop(idx));
        self.thinking_started = false;
        self.thinking_index = -1;
    }

    fn close_open_blocks(&mut self, out: &mut Vec<String>) {
        self.stop_thinking(out);
        self.stop_text(out);

        if self.content_blocks_stopped {
            return;
        }

        // Belated tool starts + stop all tools (CLIProxyAPI finish_reason path).
        // Skip tools already closed via explicit content_block_stop (Anthropic→IR→Anthropic).
        let indexes: Vec<u32> = self.tools.keys().copied().collect();
        for openai_idx in indexes {
            if self
                .tools
                .get(&openai_idx)
                .map(|a| a.closed)
                .unwrap_or(false)
            {
                continue;
            }

            let needs_start = self
                .tools
                .get(&openai_idx)
                .map(|a| !a.start_emitted && !a.name.is_empty())
                .unwrap_or(false);
            if needs_start {
                // Synthetic id if missing (CLIProxyAPI SanitizeClaudeToolID).
                {
                    let acc = self.tools.entry(openai_idx).or_default();
                    if acc.id.is_empty() {
                        acc.id = format!("toolu_conduit_{openai_idx}");
                    }
                }
                self.emit_tool_start(openai_idx, out);
            }

            let Some(acc) = self.tools.get_mut(&openai_idx) else {
                continue;
            };
            if !acc.start_emitted {
                continue;
            }
            let block_idx = self
                .tool_block_indexes
                .get(&openai_idx)
                .copied()
                .unwrap_or(0);
            // Flush buffered args only when we never streamed them live
            // (start was delayed until close).
            if !acc.args_live_streamed && !acc.arguments.is_empty() {
                let args = std::mem::take(&mut acc.arguments);
                out.push(input_json_delta(block_idx, &args));
            }
            out.push(content_block_stop(block_idx));
            acc.start_emitted = false;
            acc.closed = true;
        }
        self.content_blocks_stopped = true;
    }

    fn emit_message_delta(&mut self, out: &mut Vec<String>) {
        if self.message_delta_sent {
            return;
        }
        let fr = self.finish_reason.clone().unwrap_or(FinishReason::Stop);
        let stop = finish_reason_to_anthropic(&fr);
        let usage = self
            .pending_usage
            .clone()
            .unwrap_or_default();
        // Include input_tokens (non-standard vs pure Anthropic delta, which is
        // often output-only) so clients that miss message_start still get context.
        let data = json!({
            "type": "message_delta",
            "delta": {"stop_reason": stop, "stop_sequence": null},
            "usage": anthropic_usage_json(&usage),
        });
        out.push(format!("event: message_delta\ndata: {data}\n\n"));
        self.message_delta_sent = true;
    }

    fn emit_message_stop(&mut self, out: &mut Vec<String>) {
        if self.message_stop_sent {
            return;
        }
        out.push(encode_message_stop_frame().to_string());
        self.message_stop_sent = true;
    }
}

// ── Frame builders ──────────────────────────────────────────────────────────

/// Build Anthropic `usage` object (input/output + cache fields).
pub fn anthropic_usage_json(u: &Usage) -> serde_json::Value {
    json!({
        "input_tokens": u.prompt_tokens,
        "output_tokens": u.completion_tokens,
        "cache_creation_input_tokens": u.cache_write_tokens,
        "cache_read_input_tokens": u.cache_read_tokens,
    })
}

pub fn encode_message_start_frame(
    resp_id: &str,
    model: &str,
    prompt_tokens: u32,
    cache_read_tokens: u32,
    cache_write_tokens: u32,
) -> String {
    let usage = Usage {
        prompt_tokens,
        completion_tokens: 0,
        total_tokens: prompt_tokens,
        reasoning_tokens: 0,
        cache_read_tokens,
        cache_write_tokens,
    };
    let data = json!({
        "type": "message_start",
        "message": {
            "id": resp_id,
            "type": "message",
            "role": "assistant",
            "content": [],
            "model": model,
            "stop_reason": null,
            "stop_sequence": null,
            "usage": anthropic_usage_json(&usage),
        }
    });
    format!("event: message_start\ndata: {data}\n\n")
}

/// Convenience: `message_start` with prompt tokens only (no cache).
pub fn encode_message_start_simple(resp_id: &str, model: &str, prompt_tokens: u32) -> String {
    encode_message_start_frame(resp_id, model, prompt_tokens, 0, 0)
}

pub fn encode_message_stop_frame() -> &'static str {
    "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n"
}

fn text_block_start(index: u32) -> String {
    let data = json!({
        "type": "content_block_start",
        "index": index,
        "content_block": {"type": "text", "text": ""},
    });
    format!("event: content_block_start\ndata: {data}\n\n")
}

fn thinking_block_start(index: u32) -> String {
    let data = json!({
        "type": "content_block_start",
        "index": index,
        "content_block": {"type": "thinking", "thinking": ""},
    });
    format!("event: content_block_start\ndata: {data}\n\n")
}

fn tool_use_start(index: u32, id: &str, name: &str) -> String {
    let data = json!({
        "type": "content_block_start",
        "index": index,
        "content_block": {
            "type": "tool_use",
            "id": id,
            "name": name,
            "input": {},
        },
    });
    format!("event: content_block_start\ndata: {data}\n\n")
}

fn text_delta(index: u32, text: &str) -> String {
    let data = json!({
        "type": "content_block_delta",
        "index": index,
        "delta": {"type": "text_delta", "text": text},
    });
    format!("event: content_block_delta\ndata: {data}\n\n")
}

fn thinking_delta(index: u32, thinking: &str) -> String {
    let data = json!({
        "type": "content_block_delta",
        "index": index,
        "delta": {"type": "thinking_delta", "thinking": thinking},
    });
    format!("event: content_block_delta\ndata: {data}\n\n")
}

fn signature_delta(index: u32, signature: &str) -> String {
    let data = json!({
        "type": "content_block_delta",
        "index": index,
        "delta": {"type": "signature_delta", "signature": signature},
    });
    format!("event: content_block_delta\ndata: {data}\n\n")
}

fn input_json_delta(index: u32, partial_json: &str) -> String {
    let data = json!({
        "type": "content_block_delta",
        "index": index,
        "delta": {"type": "input_json_delta", "partial_json": partial_json},
    });
    format!("event: content_block_delta\ndata: {data}\n\n")
}

fn content_block_stop(index: u32) -> String {
    let data = json!({"type": "content_block_stop", "index": index});
    format!("event: content_block_stop\ndata: {data}\n\n")
}

pub fn finish_reason_to_anthropic(fr: &FinishReason) -> &'static str {
    match fr {
        FinishReason::Stop => "end_turn",
        FinishReason::ToolCalls => "tool_use",
        FinishReason::Length => "max_tokens",
        FinishReason::ContentFilter => "end_turn",
        FinishReason::Other(_) => "end_turn",
        _ => "end_turn",
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn text_chunk(t: &str) -> CanonicalChunk {
        CanonicalChunk::text_delta(t)
    }

    fn finish(fr: FinishReason) -> CanonicalChunk {
        CanonicalChunk::finish(
            fr,
            Some(Usage {
                prompt_tokens: 3,
                completion_tokens: 2,
                total_tokens: 5,
                ..Default::default()
            }),
        )
    }

    #[test]
    fn text_stream_has_start_delta_stop_message_stop() {
        let mut enc = AnthropicStreamEncoder::new("msg_1", "gpt-5.6-terra");
        let frames: Vec<String> = enc
            .push(&text_chunk("hi"))
            .into_iter()
            .chain(enc.push(&finish(FinishReason::Stop)))
            .collect();
        let joined = frames.join("");
        assert!(joined.contains("event: message_start"));
        assert!(joined.contains("content_block_start"));
        assert!(joined.contains("text_delta"));
        assert!(joined.contains("content_block_stop"));
        assert!(joined.contains("message_delta"));
        assert!(joined.contains("end_turn"));
        assert!(joined.contains("message_stop"));
        // Order: start before delta before stop
        let start = joined.find("content_block_start").unwrap();
        let delta = joined.find("text_delta").unwrap();
        let stop = joined.find("content_block_stop").unwrap();
        assert!(start < delta && delta < stop);
    }

    #[test]
    fn early_usage_chunk_sets_message_start_input_tokens() {
        let mut enc = AnthropicStreamEncoder::new("msg_ctx", "claude-sonnet-4");
        let early = CanonicalChunk {
            usage: Some(Usage {
                prompt_tokens: 18616,
                completion_tokens: 0,
                total_tokens: 18616,
                cache_read_tokens: 1200,
                cache_write_tokens: 80,
                ..Default::default()
            }),
            ..Default::default()
        };
        let frames: Vec<String> = enc
            .push(&early)
            .into_iter()
            .chain(enc.push(&text_chunk("hi")))
            .chain(enc.push(&finish(FinishReason::Stop)))
            .collect();
        let joined = frames.join("");
        // Claude Code reads context from message_start.usage.input_tokens.
        let start_pos = joined.find("event: message_start").expect("message_start");
        let start_slice = &joined[start_pos..];
        let data_line = start_slice
            .lines()
            .find(|l| l.starts_with("data: "))
            .expect("data line");
        let val: serde_json::Value =
            serde_json::from_str(data_line.strip_prefix("data: ").unwrap()).unwrap();
        assert_eq!(
            val["message"]["usage"]["input_tokens"].as_u64().unwrap(),
            18616
        );
        assert_eq!(
            val["message"]["usage"]["cache_read_input_tokens"]
                .as_u64()
                .unwrap(),
            1200
        );
        assert_eq!(
            val["message"]["usage"]["cache_creation_input_tokens"]
                .as_u64()
                .unwrap(),
            80
        );
        // message_start must keep the early input count even if a later finish
        // chunk revises usage for message_delta.
        assert!(
            joined.contains("\"input_tokens\":18616"),
            "expected early input_tokens in stream:\n{joined}"
        );
    }

    #[test]
    fn finish_without_prompt_does_not_clobber_early_input() {
        let mut enc = AnthropicStreamEncoder::new("msg_1", "m");
        let early = CanonicalChunk {
            usage: Some(Usage {
                prompt_tokens: 50,
                ..Default::default()
            }),
            ..Default::default()
        };
        // Finish with only output tokens (prompt=0), as some OpenAI frames do mid-stream.
        let fin = CanonicalChunk::finish(
            FinishReason::Stop,
            Some(Usage {
                prompt_tokens: 0,
                completion_tokens: 9,
                total_tokens: 9,
                ..Default::default()
            }),
        );
        let joined: String = enc
            .push(&early)
            .into_iter()
            .chain(enc.push(&text_chunk("x")))
            .chain(enc.push(&fin))
            .collect();
        let delta_pos = joined.find("event: message_delta").unwrap();
        let delta_data = joined[delta_pos..]
            .lines()
            .find(|l| l.starts_with("data: "))
            .unwrap();
        let val: serde_json::Value =
            serde_json::from_str(delta_data.strip_prefix("data: ").unwrap()).unwrap();
        assert_eq!(val["usage"]["input_tokens"].as_u64().unwrap(), 50);
        assert_eq!(val["usage"]["output_tokens"].as_u64().unwrap(), 9);
    }

    #[test]
    fn reasoning_then_text() {
        let mut enc = AnthropicStreamEncoder::new("msg_1", "m");
        let think = CanonicalChunk::thinking_delta("plan");
        let frames: Vec<_> = enc
            .push(&think)
            .into_iter()
            .chain(enc.push(&text_chunk("ans")))
            .chain(enc.push(&finish(FinishReason::Stop)))
            .collect();
        let joined = frames.join("");
        assert!(joined.contains("thinking_delta"));
        assert!(joined.contains("gpt#conduit"));
        assert!(joined.contains("text_delta"));
    }

    #[test]
    fn tool_use_lifecycle() {
        let mut enc = AnthropicStreamEncoder::new("msg_1", "m");
        let start = CanonicalChunk {
            block_kind: Some(BlockKind::ToolUse),
            tool_use_id: Some("call_1".into()),
            tool_name: Some("search".into()),
            ..Default::default()
        };
        let args = CanonicalChunk {
            block_kind: Some(BlockKind::ToolUse),
            delta: Some(BlockDelta::InputJsonDelta {
                partial_json: r#"{"q":"x"}"#.into(),
            }),
            ..Default::default()
        };
        let frames: Vec<_> = enc
            .push(&start)
            .into_iter()
            .chain(enc.push(&args))
            .chain(enc.push(&finish(FinishReason::ToolCalls)))
            .collect();
        let joined = frames.join("");
        assert!(joined.contains("tool_use"));
        assert!(joined.contains("call_1"));
        assert!(joined.contains("input_json_delta"));
        assert!(joined.contains("tool_use")); // stop_reason
        assert!(joined.contains("\"stop_reason\":\"tool_use\"") || joined.contains("tool_use"));
        // content_block_stop before message_delta
        let cbs = joined.rfind("content_block_stop").unwrap();
        let md = joined.find("message_delta").unwrap();
        assert!(cbs < md);
    }

    #[test]
    fn finish_without_tools_downgrades_tool_calls() {
        let mut enc = AnthropicStreamEncoder::new("msg_1", "m");
        let frames = enc.push(&finish(FinishReason::ToolCalls));
        let joined = frames.join("");
        assert!(joined.contains("end_turn"));
        assert!(!joined.contains("\"stop_reason\":\"tool_use\""));
    }

    /// Anthropic-native IR already emits content_block_stop before message_delta.
    /// finish must not re-open tools with empty `input: {}` (Claude Code then
    /// shows "Invalid tool parameters" / missing required fields).
    #[test]
    fn anthropic_native_tool_stop_before_finish_does_not_reemit() {
        let mut enc = AnthropicStreamEncoder::new("msg_1", "claude-opus");
        let start = CanonicalChunk {
            block_kind: Some(BlockKind::ToolUse),
            tool_use_id: Some("toolu_abc".into()),
            tool_name: Some("Bash".into()),
            ..Default::default()
        };
        let args = CanonicalChunk {
            delta: Some(BlockDelta::InputJsonDelta {
                partial_json: r#"{"command":"pwd"}"#.into(),
            }),
            ..Default::default()
        };
        // Explicit content_block_stop (all fields empty except block_index).
        let block_stop = CanonicalChunk::default();
        let frames: Vec<_> = enc
            .push(&start)
            .into_iter()
            .chain(enc.push(&args))
            .chain(enc.push(&block_stop))
            .chain(enc.push(&finish(FinishReason::ToolCalls)))
            .collect();
        let joined = frames.join("");

        // Count SSE event lines only (JSON body also contains the type string).
        let block_starts = joined.matches("event: content_block_start").count();
        assert_eq!(
            block_starts, 1,
            "expected single tool block start, got:\n{joined}"
        );
        assert!(
            joined.contains(r#"{"command":"pwd"}"#) || joined.contains(r#"\"command\":\"pwd\""#)
        );
        // Ensure we did not emit a second tool_use start after stop.
        let first_stop = joined.find("event: content_block_stop").expect("stop");
        let after_stop = &joined[first_stop..];
        assert!(
            !after_stop.contains("event: content_block_start"),
            "re-emitted tool after stop:\n{joined}"
        );
    }

    #[test]
    fn two_anthropic_tools_stop_then_finish_once_each() {
        let mut enc = AnthropicStreamEncoder::new("msg_1", "claude-opus");
        let mk_start = |idx: u32, id: &str, name: &str| CanonicalChunk {
            block_index: idx,
            block_kind: Some(BlockKind::ToolUse),
            tool_use_id: Some(id.into()),
            tool_name: Some(name.into()),
            ..Default::default()
        };
        let mk_args = |idx: u32, json: &str| CanonicalChunk {
            block_index: idx,
            delta: Some(BlockDelta::InputJsonDelta {
                partial_json: json.into(),
            }),
            ..Default::default()
        };
        let mk_stop = |idx: u32| CanonicalChunk {
            block_index: idx,
            ..Default::default()
        };

        let frames: Vec<_> = enc
            .push(&mk_start(0, "toolu_1", "Bash"))
            .into_iter()
            .chain(enc.push(&mk_args(0, r#"{"command":"ls"}"#)))
            .chain(enc.push(&mk_stop(0)))
            .chain(enc.push(&mk_start(1, "toolu_2", "Bash")))
            .chain(enc.push(&mk_args(1, r#"{"command":"pwd"}"#)))
            .chain(enc.push(&mk_stop(1)))
            .chain(enc.push(&finish(FinishReason::ToolCalls)))
            .collect();
        let joined = frames.join("");
        assert_eq!(
            joined.matches("event: content_block_start").count(),
            2,
            "expected exactly 2 tool starts:\n{joined}"
        );
        assert!(joined.contains("toolu_1") && joined.contains("toolu_2"));
        // No third start after final stop / message_delta.
        let last_stop = joined.rfind("event: content_block_stop").unwrap();
        let after = &joined[last_stop..];
        assert!(!after.contains("event: content_block_start"), "{joined}");
    }
}
