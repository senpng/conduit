pub mod decode_request;
pub mod decode_response;
pub mod encode_request;
pub mod stream;

use conduit_ir::{
    canonical::{
        CanonicalChatRequest, CanonicalChatResponse, CanonicalChunk, CanonicalContent,
        FinishReason, ToolChoice,
    },
    error::CodecError,
    loss::LossReport,
};
use serde_json::{json, Value};
pub use stream::{encode_message_start, encode_message_stop, AnthropicStreamEncoder};

use crate::WireCodec;

pub struct AnthropicCodec;

impl WireCodec for AnthropicCodec {
    fn encode_request(req: &CanonicalChatRequest, s: bool) -> (Value, LossReport) {
        let mut cloned = req.clone();
        let loss = degrade_anyof(&mut cloned);
        (encode_request::encode_request(&cloned, s), loss)
    }

    fn decode_request(
        body: Value,
        alias: String,
        stream: bool,
        request_id: String,
        key_id: String,
    ) -> Result<CanonicalChatRequest, CodecError> {
        decode_request::decode_request(body, alias, stream, request_id, key_id)
    }

    fn decode_response(
        body: Value,
        alias: &str,
    ) -> Result<(CanonicalChatResponse, LossReport), CodecError> {
        decode_response::decode_response(body, alias)
    }

    fn encode_response(resp: &CanonicalChatResponse) -> Value {
        let content: Vec<Value> = resp
            .choices
            .first()
            .map(|m| &m.content)
            .unwrap_or(&vec![])
            .iter()
            .filter_map(|c| match c {
                CanonicalContent::Text { text } => Some(json!({"type": "text", "text": text})),
                CanonicalContent::ToolUse { id, name, input } => Some(json!({
                    "type": "tool_use",
                    "id": id,
                    "name": name,
                    "input": input,
                })),
                CanonicalContent::Thinking {
                    thinking,
                    signature,
                } => {
                    let mut v = json!({"type": "thinking", "thinking": thinking});
                    if let Some(sig) = signature {
                        v["signature"] = json!(sig);
                    }
                    Some(v)
                }
                _ => None,
            })
            .collect();

        let stop_reason = match &resp.finish_reason {
            FinishReason::ToolCalls => "tool_use",
            FinishReason::Length => "max_tokens",
            _ => "end_turn",
        };

        json!({
            "id": resp.id,
            "type": "message",
            "role": "assistant",
            "content": content,
            "model": resp.model,
            "stop_reason": stop_reason,
            "stop_sequence": null,
            "usage": stream::anthropic_usage_json(&resp.usage),
        })
    }

    fn encode_chunk(chunk: &CanonicalChunk, resp_id: &str) -> (Option<String>, LossReport) {
        (stream::encode_chunk(chunk, resp_id), LossReport::default())
    }

    fn decode_chunk(data: &str) -> Result<(Vec<CanonicalChunk>, LossReport), CodecError> {
        let mut state = stream::AnthropicStreamState::new();
        Self::decode_chunk_stateful(&mut state, data)
    }

    type StreamState = stream::AnthropicStreamState;

    fn decode_chunk_stateful(
        state: &mut Self::StreamState,
        data: &str,
    ) -> Result<(Vec<CanonicalChunk>, LossReport), CodecError> {
        if data.trim().is_empty() {
            return Ok((vec![], LossReport::default()));
        }
        let val: Value = serde_json::from_str(data.trim())?;
        // Prefer JSON `type` (Anthropic puts it in the data payload). Fall back
        // is unused today because upstream SSE only forwards data lines.
        let event_type = val["type"].as_str().unwrap_or("");
        let chunks = stream::decode_event(state, event_type, &val)?;
        Ok((chunks, LossReport::default()))
    }

    fn error_body(type_: &str, _code: Option<&str>, message: &str) -> Value {
        let normalized = match type_ {
            "invalid_request_error"
            | "authentication_error"
            | "permission_error"
            | "not_found_error"
            | "rate_limit_error"
            | "api_error"
            | "overloaded_error" => type_,
            _ => "api_error",
        };
        json!({
            "type": "error",
            "error": {"type": normalized, "message": message}
        })
    }

    fn stream_error_sse(message: &str) -> String {
        format!(
            "event: error\ndata: {}\n\n",
            json!({"type": "error", "error": {"type": "api_error", "message": message}})
        )
    }
}

/// Degrade `AnyOf` → `Required` for Anthropic (maps to `any` tool_choice type).
/// Returns the loss.
fn degrade_anyof(req: &mut CanonicalChatRequest) -> LossReport {
    let mut loss = LossReport::default();
    if let Some(ToolChoice::AnyOf { names }) = &req.tool_choice {
        let original = format!("AnyOf({:?})", names);
        loss.add(
            "tool_choice",
            original,
            "Required (Anthropic: any)",
            "Anthropic does not support AnyOf; degraded to Required/any",
        );
        req.tool_choice = Some(ToolChoice::Required);
    }
    loss
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use conduit_ir::canonical::{CanonicalMessage, Usage};

    use super::*;

    #[test]
    fn encode_decode_response_roundtrip() {
        let resp = CanonicalChatResponse {
            id: "msg_1".into(),
            request_id: String::new(),
            model: "claude-3-5-sonnet".into(),
            choices: vec![CanonicalMessage::assistant("Hello!")],
            finish_reason: FinishReason::Stop,
            usage: Usage {
                prompt_tokens: 10,
                completion_tokens: 5,
                total_tokens: 15,
                cache_read_tokens: 4,
                cache_write_tokens: 2,
                ..Default::default()
            },
            created_at: chrono::Utc::now(),
        };
        let wire = AnthropicCodec::encode_response(&resp);
        assert_eq!(wire["id"].as_str().unwrap(), "msg_1");
        assert_eq!(wire["stop_reason"].as_str().unwrap(), "end_turn");
        assert_eq!(wire["content"][0]["text"].as_str().unwrap(), "Hello!");
        assert_eq!(wire["usage"]["input_tokens"].as_u64().unwrap(), 10);
        assert_eq!(wire["usage"]["output_tokens"].as_u64().unwrap(), 5);
        assert_eq!(
            wire["usage"]["cache_read_input_tokens"].as_u64().unwrap(),
            4
        );
        assert_eq!(
            wire["usage"]["cache_creation_input_tokens"]
                .as_u64()
                .unwrap(),
            2
        );
    }

    #[test]
    fn stream_stateful_decode_preserves_input_tokens_and_reencodes() {
        use serde_json::json;

        let frames = [
            json!({
                "type": "message_start",
                "message": {
                    "id": "msg_up",
                    "type": "message",
                    "role": "assistant",
                    "content": [],
                    "model": "claude-sonnet-4",
                    "stop_reason": null,
                    "stop_sequence": null,
                    "usage": {
                        "input_tokens": 1234,
                        "output_tokens": 0,
                        "cache_read_input_tokens": 100,
                        "cache_creation_input_tokens": 0
                    }
                }
            }),
            json!({
                "type": "content_block_start",
                "index": 0,
                "content_block": {"type": "text", "text": ""}
            }),
            json!({
                "type": "content_block_delta",
                "index": 0,
                "delta": {"type": "text_delta", "text": "ok"}
            }),
            json!({"type": "content_block_stop", "index": 0}),
            json!({
                "type": "message_delta",
                "delta": {"stop_reason": "end_turn", "stop_sequence": null},
                "usage": {"output_tokens": 7}
            }),
            json!({"type": "message_stop"}),
        ];

        let mut state = stream::AnthropicStreamState::new();
        let mut enc = AnthropicStreamEncoder::new("msg_client", "claude-sonnet-4");
        let mut out = String::new();
        for frame in frames {
            let data = serde_json::to_string(&frame).unwrap();
            let (chunks, _) = AnthropicCodec::decode_chunk_stateful(&mut state, &data).unwrap();
            for chunk in chunks {
                for f in enc.push(&chunk) {
                    out.push_str(&f);
                }
            }
        }
        for f in enc.finish() {
            out.push_str(&f);
        }

        let start_pos = out.find("event: message_start").expect("client message_start");
        let start_data = out[start_pos..]
            .lines()
            .find(|l| l.starts_with("data: "))
            .unwrap();
        let start_val: Value =
            serde_json::from_str(start_data.strip_prefix("data: ").unwrap()).unwrap();
        assert_eq!(
            start_val["message"]["usage"]["input_tokens"]
                .as_u64()
                .unwrap(),
            1234,
            "Claude Code context depends on message_start input_tokens; got:\n{out}"
        );
        assert_eq!(
            start_val["message"]["usage"]["cache_read_input_tokens"]
                .as_u64()
                .unwrap(),
            100
        );

        let delta_pos = out.find("event: message_delta").unwrap();
        let delta_data = out[delta_pos..]
            .lines()
            .find(|l| l.starts_with("data: "))
            .unwrap();
        let delta_val: Value =
            serde_json::from_str(delta_data.strip_prefix("data: ").unwrap()).unwrap();
        assert_eq!(delta_val["usage"]["input_tokens"].as_u64().unwrap(), 1234);
        assert_eq!(delta_val["usage"]["output_tokens"].as_u64().unwrap(), 7);
    }

    #[test]
    fn error_body_normalizes_unknown_type() {
        let v = AnthropicCodec::error_body("unknown_type", None, "oops");
        assert_eq!(v["error"]["type"].as_str().unwrap(), "api_error");
    }

    #[test]
    fn stream_error_sse_format() {
        let s = AnthropicCodec::stream_error_sse("bad input");
        assert!(s.starts_with("event: error\n"));
        assert!(s.contains("api_error"));
        assert!(s.contains("bad input"));
    }

    #[test]
    fn anyof_degrade_recorded() {
        use conduit_ir::canonical::ToolDef;
        let mut req = CanonicalChatRequest::new("c3", vec![CanonicalMessage::user("hi")]);
        req.tools = vec![ToolDef {
            name: "fn".into(),
            description: None,
            parameters: serde_json::json!({"type": "object"}),
        }];
        req.tool_choice = Some(ToolChoice::AnyOf {
            names: vec!["fn".into()],
        });
        let loss = degrade_anyof(&mut req);
        assert!(!loss.is_empty());
        assert_eq!(loss.warnings[0].field, "tool_choice");
    }

    /// Reconstruct tool inputs from client-facing Anthropic SSE (what Claude Code sees).
    fn reconstruct_tool_inputs_from_sse(sse: &str) -> Vec<(String, String, String)> {
        // (id, name, concatenated partial_json)
        let mut tools: Vec<(String, String, String)> = Vec::new();
        let mut by_index: std::collections::HashMap<u32, usize> = std::collections::HashMap::new();

        for block in sse.split("\n\n") {
            let data_line = block.lines().find(|l| l.starts_with("data: "));
            let Some(data_line) = data_line else {
                continue;
            };
            let Ok(val) = serde_json::from_str::<Value>(data_line.strip_prefix("data: ").unwrap())
            else {
                continue;
            };
            match val["type"].as_str() {
                Some("content_block_start") => {
                    let idx = val["index"].as_u64().unwrap_or(0) as u32;
                    let cb = &val["content_block"];
                    if cb["type"].as_str() == Some("tool_use") {
                        let id = cb["id"].as_str().unwrap_or("").to_string();
                        let name = cb["name"].as_str().unwrap_or("").to_string();
                        by_index.insert(idx, tools.len());
                        tools.push((id, name, String::new()));
                    }
                }
                Some("content_block_delta") => {
                    let idx = val["index"].as_u64().unwrap_or(0) as u32;
                    if val["delta"]["type"].as_str() == Some("input_json_delta") {
                        if let Some(&ti) = by_index.get(&idx) {
                            if let Some(p) = val["delta"]["partial_json"].as_str() {
                                tools[ti].2.push_str(p);
                            }
                        }
                    }
                }
                _ => {}
            }
        }
        tools
    }

    /// Full Anthropic→IR→Anthropic path used by `/v1/messages` + claude-oauth stream.
    fn reencode_upstream_frames(frames: &[Value]) -> String {
        let mut state = stream::AnthropicStreamState::new();
        let mut enc = AnthropicStreamEncoder::new("msg_client", "claude-sonnet-4");
        let mut out = String::new();
        for frame in frames {
            let data = serde_json::to_string(frame).unwrap();
            let (chunks, _) = AnthropicCodec::decode_chunk_stateful(&mut state, &data).unwrap();
            for chunk in chunks {
                for f in enc.push(&chunk) {
                    out.push_str(&f);
                }
            }
        }
        for f in enc.finish() {
            out.push_str(&f);
        }
        out
    }

    #[test]
    fn edit_tool_multi_delta_input_survives_ir_reencode() {
        // Claude Code Edit params often arrive as many small input_json_delta frames.
        let old = "fn handle_event(e: Event) {\n    match e {\n        Event::Key(k) => {}\n    }\n}\n";
        let new = "fn handle_event(e: Event) {\n    match e {\n        Event::Key(k) => { log(k); }\n    }\n}\n";
        let full_input = json!({
            "file_path": "crates/conduitctl/src/tui/event.rs",
            "old_string": old,
            "new_string": new,
        });
        let full = serde_json::to_string(&full_input).unwrap();
        // Split like real Anthropic streaming (uneven chunk sizes + JSON special chars).
        let parts: Vec<&str> = full
            .as_bytes()
            .chunks(17)
            .map(|c| std::str::from_utf8(c).unwrap())
            .collect();

        let mut frames = vec![
            json!({
                "type": "message_start",
                "message": {
                    "id": "msg_up",
                    "type": "message",
                    "role": "assistant",
                    "content": [],
                    "model": "claude-sonnet-4",
                    "stop_reason": null,
                    "stop_sequence": null,
                    "usage": {"input_tokens": 900, "output_tokens": 0}
                }
            }),
            json!({
                "type": "content_block_start",
                "index": 0,
                "content_block": {
                    "type": "tool_use",
                    "id": "toolu_edit1",
                    "name": "Edit",
                    "input": {}
                }
            }),
        ];
        for p in &parts {
            frames.push(json!({
                "type": "content_block_delta",
                "index": 0,
                "delta": {"type": "input_json_delta", "partial_json": p}
            }));
        }
        frames.push(json!({"type": "content_block_stop", "index": 0}));
        frames.push(json!({
            "type": "message_delta",
            "delta": {"stop_reason": "tool_use", "stop_sequence": null},
            "usage": {"output_tokens": 40}
        }));
        frames.push(json!({"type": "message_stop"}));

        let out = reencode_upstream_frames(&frames);
        let tools = reconstruct_tool_inputs_from_sse(&out);
        assert_eq!(tools.len(), 1, "expected single Edit tool_use, got:\n{out}");
        assert_eq!(tools[0].0, "toolu_edit1");
        assert_eq!(tools[0].1, "Edit");
        let parsed: Value = serde_json::from_str(&tools[0].2).unwrap_or_else(|e| {
            panic!("client Edit input is not valid JSON ({e}): {}\nSSE:\n{out}", tools[0].2)
        });
        assert_eq!(
            parsed["file_path"].as_str().unwrap(),
            "crates/conduitctl/src/tui/event.rs"
        );
        assert_eq!(parsed["old_string"].as_str().unwrap(), old);
        assert_eq!(parsed["new_string"].as_str().unwrap(), new);
        // Must not re-open a second empty tool after stop (Claude Code: invalid params).
        assert_eq!(
            out.matches("event: content_block_start").count(),
            1,
            "duplicate tool starts would break Edit:\n{out}"
        );
        assert!(
            out.contains("\"stop_reason\":\"tool_use\""),
            "stop_reason should stay tool_use:\n{out}"
        );
    }

    #[test]
    fn thinking_then_edit_preserves_real_signature_and_edit_args() {
        // Valid-looking Anthropic thinking signature (E-form base64).
        let real_sig = "EAB4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHg=";
        let edit_json = r#"{"file_path":"a.rs","old_string":"x","new_string":"y"}"#;
        let frames = [
            json!({
                "type": "message_start",
                "message": {
                    "id": "msg_up",
                    "type": "message",
                    "role": "assistant",
                    "content": [],
                    "model": "claude-sonnet-4",
                    "stop_reason": null,
                    "stop_sequence": null,
                    "usage": {"input_tokens": 10, "output_tokens": 0}
                }
            }),
            json!({
                "type": "content_block_start",
                "index": 0,
                "content_block": {"type": "thinking", "thinking": ""}
            }),
            json!({
                "type": "content_block_delta",
                "index": 0,
                "delta": {"type": "thinking_delta", "thinking": "plan edit"}
            }),
            json!({
                "type": "content_block_delta",
                "index": 0,
                "delta": {"type": "signature_delta", "signature": real_sig}
            }),
            json!({"type": "content_block_stop", "index": 0}),
            json!({
                "type": "content_block_start",
                "index": 1,
                "content_block": {
                    "type": "tool_use",
                    "id": "toolu_e2",
                    "name": "Edit",
                    "input": {}
                }
            }),
            json!({
                "type": "content_block_delta",
                "index": 1,
                "delta": {"type": "input_json_delta", "partial_json": edit_json}
            }),
            json!({"type": "content_block_stop", "index": 1}),
            json!({
                "type": "message_delta",
                "delta": {"stop_reason": "tool_use", "stop_sequence": null},
                "usage": {"output_tokens": 12}
            }),
            json!({"type": "message_stop"}),
        ];

        let out = reencode_upstream_frames(&frames);
        let tools = reconstruct_tool_inputs_from_sse(&out);
        assert_eq!(tools.len(), 1, "Edit must survive after thinking:\n{out}");
        assert_eq!(tools[0].1, "Edit");
        let parsed: Value = serde_json::from_str(&tools[0].2).expect("edit json");
        assert_eq!(parsed["file_path"], "a.rs");
        assert_eq!(parsed["old_string"], "x");
        assert_eq!(parsed["new_string"], "y");

        // Anthropic-native signature must pass through; do not stamp gpt#conduit on stop
        // (that poisons multi-turn OAuth history when sanitize drops invalid thinking).
        assert!(
            out.contains(real_sig),
            "real Anthropic signature must be preserved:\n{out}"
        );
        assert!(
            !out.contains("gpt#conduit"),
            "must not overwrite Anthropic signature with gpt#conduit:\n{out}"
        );
        assert_eq!(
            out.matches("signature_delta").count(),
            1,
            "expected exactly one signature_delta:\n{out}"
        );
    }

    #[test]
    fn two_edit_tools_both_inputs_intact() {
        let frames = [
            json!({
                "type": "message_start",
                "message": {
                    "id": "msg_up",
                    "type": "message",
                    "role": "assistant",
                    "content": [],
                    "model": "claude-sonnet-4",
                    "stop_reason": null,
                    "stop_sequence": null,
                    "usage": {"input_tokens": 1, "output_tokens": 0}
                }
            }),
            json!({
                "type": "content_block_start",
                "index": 0,
                "content_block": {
                    "type": "tool_use",
                    "id": "toolu_1",
                    "name": "Edit",
                    "input": {}
                }
            }),
            json!({
                "type": "content_block_delta",
                "index": 0,
                "delta": {
                    "type": "input_json_delta",
                    "partial_json": r#"{"file_path":"a.rs","old_string":"1","new_string":"2"}"#
                }
            }),
            json!({"type": "content_block_stop", "index": 0}),
            json!({
                "type": "content_block_start",
                "index": 1,
                "content_block": {
                    "type": "tool_use",
                    "id": "toolu_2",
                    "name": "Edit",
                    "input": {}
                }
            }),
            json!({
                "type": "content_block_delta",
                "index": 1,
                "delta": {
                    "type": "input_json_delta",
                    "partial_json": r#"{"file_path":"b.rs","old_string":"3","new_string":"4"}"#
                }
            }),
            json!({"type": "content_block_stop", "index": 1}),
            json!({
                "type": "message_delta",
                "delta": {"stop_reason": "tool_use", "stop_sequence": null},
                "usage": {"output_tokens": 8}
            }),
            json!({"type": "message_stop"}),
        ];
        let out = reencode_upstream_frames(&frames);
        let tools = reconstruct_tool_inputs_from_sse(&out);
        assert_eq!(tools.len(), 2, "expected 2 Edit tools:\n{out}");
        let p0: Value = serde_json::from_str(&tools[0].2).unwrap();
        let p1: Value = serde_json::from_str(&tools[1].2).unwrap();
        assert_eq!(p0["file_path"], "a.rs");
        assert_eq!(p1["file_path"], "b.rs");
        assert_eq!(out.matches("event: content_block_start").count(), 2);
    }
}
