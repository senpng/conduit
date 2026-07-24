//! Stateful decode of Codex / OpenAI Responses SSE events into IR chunks.

use conduit_ir::canonical::{
    BlockDelta, BlockKind, CanonicalChunk, FinishReason,
};
use serde_json::Value;

use super::helpers::parse_usage;

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
pub(crate) fn decode_responses_sse_event_stateful(
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

