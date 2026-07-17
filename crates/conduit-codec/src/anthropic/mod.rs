pub mod decode_request;
pub mod decode_response;
pub mod encode_request;
pub mod stream;

pub use stream::{AnthropicStreamEncoder, encode_message_start, encode_message_stop};

use conduit_ir::{
    canonical::{
        CanonicalChatRequest, CanonicalChatResponse, CanonicalChunk, CanonicalContent,
        FinishReason, ToolChoice,
    },
    error::CodecError,
    loss::LossReport,
};
use serde_json::{json, Value};

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
        let resp = decode_response::decode_response(body, alias)?;
        Ok((resp, LossReport::default()))
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
            "usage": {
                "input_tokens": resp.usage.prompt_tokens,
                "output_tokens": resp.usage.completion_tokens,
            }
        })
    }

    fn encode_chunk(chunk: &CanonicalChunk, resp_id: &str) -> (Option<String>, LossReport) {
        (stream::encode_chunk(chunk, resp_id), LossReport::default())
    }

    fn decode_chunk(data: &str) -> Result<(Vec<CanonicalChunk>, LossReport), CodecError> {
        if data.trim().is_empty() {
            return Ok((vec![], LossReport::default()));
        }
        let val: Value = serde_json::from_str(data.trim())?;
        let event_type = val["type"].as_str().unwrap_or("");
        let mut state = stream::AnthropicStreamState::new();
        let chunks = stream::decode_event(&mut state, event_type, &val)?;
        Ok((chunks, LossReport::default()))
    }

    type StreamState = ();

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
                ..Default::default()
            },
            created_at: chrono::Utc::now(),
        };
        let wire = AnthropicCodec::encode_response(&resp);
        assert_eq!(wire["id"].as_str().unwrap(), "msg_1");
        assert_eq!(wire["stop_reason"].as_str().unwrap(), "end_turn");
        assert_eq!(wire["content"][0]["text"].as_str().unwrap(), "Hello!");
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
