pub mod decode_request;
pub mod decode_response;
pub mod encode_request;
pub mod stream;

use conduit_ir::{
    canonical::{CanonicalChatRequest, CanonicalChatResponse, CanonicalChunk, ToolChoice},
    error::CodecError,
    loss::LossReport,
};
use serde_json::{json, Value};

use crate::WireCodec;

pub struct OpenAiCodec;

impl WireCodec for OpenAiCodec {
    fn encode_request(req: &CanonicalChatRequest, stream: bool) -> (Value, LossReport) {
        let mut cloned = req.clone();
        let loss = degrade_tool_choice(&mut cloned);
        (encode_request::encode_request(&cloned, stream), loss)
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
        use conduit_ir::canonical::{CanonicalContent, FinishReason};

        let first = resp.choices.first();
        let tool_calls: Vec<Value> = first
            .map(|m| &m.content)
            .unwrap_or(&vec![])
            .iter()
            .filter_map(|c| {
                if let CanonicalContent::ToolUse { id, name, input } = c {
                    Some(json!({
                        "id": id,
                        "type": "function",
                        "function": {"name": name, "arguments": input.to_string()}
                    }))
                } else {
                    None
                }
            })
            .collect();

        let text: String = first
            .map(|m| &m.content)
            .unwrap_or(&vec![])
            .iter()
            .filter_map(|c| {
                if let CanonicalContent::Text { text } = c {
                    Some(text.as_str())
                } else {
                    None
                }
            })
            .collect::<Vec<_>>()
            .join("");

        let message = if !tool_calls.is_empty() {
            json!({"role": "assistant", "content": null, "tool_calls": tool_calls})
        } else {
            json!({"role": "assistant", "content": text})
        };

        let fr_str = match &resp.finish_reason {
            FinishReason::Stop => "stop",
            FinishReason::Length => "length",
            FinishReason::ToolCalls => "tool_calls",
            FinishReason::ContentFilter => "content_filter",
            FinishReason::Other(s) => s.as_str(),
            _ => "stop",
        };

        json!({
            "id": resp.id,
            "object": "chat.completion",
            "created": resp.created_at.timestamp(),
            "model": resp.model,
            "choices": [{
                "index": 0,
                "message": message,
                "finish_reason": fr_str,
            }],
            "usage": {
                "prompt_tokens": resp.usage.prompt_tokens,
                "completion_tokens": resp.usage.completion_tokens,
                "total_tokens": resp.usage.total_tokens,
            }
        })
    }

    fn encode_chunk(chunk: &CanonicalChunk, resp_id: &str) -> (Option<String>, LossReport) {
        (stream::encode_chunk(chunk, resp_id), LossReport::default())
    }

    fn decode_chunk(data: &str) -> Result<(Vec<CanonicalChunk>, LossReport), CodecError> {
        let chunks = stream::decode_chunks(data)?;
        Ok((chunks, LossReport::default()))
    }

    type StreamState = ();

    fn error_body(type_: &str, code: Option<&str>, message: &str) -> Value {
        let mut error = json!({"type": type_, "message": message});
        if let Some(c) = code {
            error["code"] = json!(c);
        }
        json!({"error": error})
    }

    fn stream_error_sse(message: &str) -> String {
        format!("data: {}\n\n", json!({"error": {"message": message}}))
    }
}

impl OpenAiCodec {
    /// Encode a stream chunk with an explicit model / route alias.
    ///
    /// Prefer [`OpenAiStreamEncoder`] on the gateway path for CLIProxyAPI-parity
    /// (fixed `created`, role kickoff, `[DONE]`).
    pub fn encode_chunk_with_model(
        chunk: &CanonicalChunk,
        resp_id: &str,
        model: &str,
    ) -> (Option<String>, LossReport) {
        (
            stream::encode_chunk_with_model(chunk, resp_id, model),
            LossReport::default(),
        )
    }
}

pub use stream::OpenAiStreamEncoder;

/// If `tool_choice` is `AnyOf`, degrade to `Required` on the cloned request
/// and return the loss.
fn degrade_tool_choice(req: &mut CanonicalChatRequest) -> LossReport {
    let mut loss = LossReport::default();
    if let Some(ToolChoice::AnyOf { names }) = &req.tool_choice {
        let original = format!("AnyOf({:?})", names);
        loss.add(
            "tool_choice",
            original,
            "Required",
            "OpenAI does not support AnyOf tool_choice; degraded to Required",
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
    use conduit_ir::canonical::{CanonicalMessage, ToolChoice, ToolDef};

    use super::*;

    #[test]
    fn anyof_degraded_with_loss_report() {
        let req_orig = {
            let mut r = CanonicalChatRequest::new("gpt-4o", vec![CanonicalMessage::user("hi")]);
            r.tools = vec![ToolDef {
                name: "search".into(),
                description: None,
                parameters: json!({"type": "object"}),
            }];
            r.tool_choice = Some(ToolChoice::AnyOf {
                names: vec!["search".into()],
            });
            r
        };
        let (wire, loss) = OpenAiCodec::encode_request(&req_orig, false);
        assert_eq!(wire["tool_choice"].as_str().unwrap(), "required");
        // The original request is not mutated.
        assert!(matches!(
            req_orig.tool_choice,
            Some(ToolChoice::AnyOf { .. })
        ));
        // Loss is recorded in the returned report.
        assert!(!loss.is_empty());
        assert_eq!(loss.warnings[0].field, "tool_choice");
    }

    #[test]
    fn loss_report_populated_on_anyof_degrade() {
        let mut req = CanonicalChatRequest::new("gpt-4o", vec![CanonicalMessage::user("hi")]);
        req.tools = vec![ToolDef {
            name: "calc".into(),
            description: None,
            parameters: json!({"type": "object"}),
        }];
        req.tool_choice = Some(ToolChoice::AnyOf {
            names: vec!["calc".into()],
        });
        let loss = degrade_tool_choice(&mut req);
        assert!(!loss.is_empty());
        assert_eq!(loss.warnings[0].field, "tool_choice");
        assert_eq!(loss.warnings[0].degraded_to, "Required");
    }

    #[test]
    fn roundtrip_encode_decode_response() {
        use conduit_ir::canonical::{CanonicalChatResponse, FinishReason, Usage};
        let resp = CanonicalChatResponse {
            id: "chatcmpl-1".into(),
            request_id: String::new(),
            model: "gpt-4o".into(),
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
        let wire = OpenAiCodec::encode_response(&resp);
        assert_eq!(
            wire["choices"][0]["message"]["content"].as_str().unwrap(),
            "Hello!"
        );
        assert_eq!(wire["usage"]["total_tokens"].as_u64().unwrap(), 15);
    }
}
