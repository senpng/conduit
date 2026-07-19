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
}
