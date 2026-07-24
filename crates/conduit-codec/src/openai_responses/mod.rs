//! Minimal OpenAI Responses API codec (Codex OAuth + Grok Responses).
//!
//! Wire shape (request):
//! ```json
//! { "model": "...", "input": [...], "stream": bool, "store": false, "tools": [...] }
//! ```
//!
//! ChatGPT-account Codex (`chatgpt.com/backend-api/codex`) has extra constraints —
//! see [`apply_codex_chatgpt_account_body`] (aligned with CLIProxyAPI).
//!
//! Response (non-stream): full response object with `output` array.
//! Stream: SSE events with `type` field (`response.output_text.delta`, etc.).

mod body;
mod codec;
mod continuation;
mod helpers;
mod stream_decode;
mod stream_encode;

pub use body::{
    apply_codex_chatgpt_account_body, prepare_responses_compact_body,
    sanitize_responses_reasoning_encrypted_content,
};
pub use codec::OpenAIResponsesCodec;
pub use continuation::{
    can_reset_responses_continuation, merge_responses_continuation, prepare_responses_continuation,
    reset_responses_continuation, response_output_items, responses_store_enabled,
    ResponsesContinuation, ResponsesContinuationRequest,
};
pub use stream_decode::ResponsesStreamState;
pub use stream_encode::ResponsesStreamEncoder;

#[cfg(test)]
mod tests {
    use super::helpers::content_to_text;
    use super::*;
    use crate::WireCodec;
    use conduit_ir::canonical::{
        BlockDelta, BlockKind, CanonicalChatRequest, CanonicalChatResponse, CanonicalChunk,
        CanonicalContent, CanonicalMessage, FinishReason, Role, ToolChoice, Usage,
    };
    use serde_json::{json, Value};

    #[test]
    fn encode_simple_request() {
        let req = CanonicalChatRequest::new(
            "gpt-5",
            vec![
                CanonicalMessage::system("sys"),
                CanonicalMessage::user("hello"),
            ],
        );
        let (wire, _) = OpenAIResponsesCodec::encode_request(&req, false);
        assert_eq!(wire["model"], "gpt-5");
        assert!(wire["input"].as_array().unwrap().len() >= 2);
        assert_eq!(wire["stream"], false);
        assert_eq!(wire["store"], false);
    }

    #[test]
    fn decode_request_preserves_input_tools_and_reasoning() {
        let body = json!({
            "model": "gpt-5",
            "instructions": "Be concise.",
            "input": [
                {"type": "message", "role": "user", "content": [
                    {"type": "input_text", "text": "Find the answer"}
                ]},
                {"type": "function_call", "call_id": "call_1", "name": "search", "arguments": "{\"q\":\"conduit\"}"},
                {"type": "function_call_output", "call_id": "call_1", "output": "found it"}
            ],
            "tools": [{
                "type": "function",
                "name": "search",
                "description": "Search documents",
                "parameters": {"type": "object"}
            }],
            "tool_choice": {"type": "function", "name": "search"},
            "reasoning": {"effort": "high"},
            "max_output_tokens": 128,
            "service_tier": "priority",
            "stream": true
        });
        let req = OpenAIResponsesCodec::decode_request(
            body,
            "gpt-5".into(),
            true,
            "req_1".into(),
            "key_1".into(),
        )
        .expect("Responses request must decode");

        assert_eq!(req.id, "req_1");
        assert_eq!(req.messages.len(), 4);
        assert_eq!(req.messages[0].role, Role::System);
        assert_eq!(req.messages[1].role, Role::User);
        assert!(matches!(
            req.messages[2].content.as_slice(),
            [CanonicalContent::ToolUse { id, name, .. }] if id == "call_1" && name == "search"
        ));
        assert!(matches!(
            req.messages[3].content.as_slice(),
            [CanonicalContent::ToolResult { tool_use_id, .. }] if tool_use_id == "call_1"
        ));
        assert_eq!(req.tools[0].name, "search");
        assert!(matches!(
            req.tool_choice,
            Some(ToolChoice::Tool { ref name }) if name == "search"
        ));
        assert_eq!(req.sampling.reasoning_effort.as_deref(), Some("high"));
        assert_eq!(req.sampling.max_tokens, Some(128));
        assert_eq!(req.sampling.service_tier.as_deref(), Some("priority"));
        assert!(req.stream);
    }

    #[test]
    fn decode_request_rejects_orphaned_tool_output_for_stateless_gateway() {
        let body = json!({
            "model": "gpt-5",
            "previous_response_id": "resp_old",
            "input": [{
                "type": "function_call_output",
                "call_id": "call_1",
                "output": "found it"
            }]
        });

        let error = OpenAIResponsesCodec::decode_request(
            body,
            "gpt-5".into(),
            false,
            "req_1".into(),
            "key_1".into(),
        )
        .expect_err("unresolved tool output must be rejected locally");
        assert!(error.to_string().contains("has no preceding function_call"));
    }

    #[test]
    fn continuation_merges_plain_text_history_in_protocol_layer() {
        let continuation = ResponsesContinuation::new(
            json!([{"type": "message", "role": "user", "content": "remember this"}]),
            vec![json!({
                "type": "message",
                "role": "assistant",
                "content": [{"type": "output_text", "text": "I remember"}]
            })],
        );
        let request = json!({
            "previous_response_id": "resp_1",
            "input": "what did I ask you to remember?"
        });
        let ResponsesContinuationRequest::Incremental { body, .. } =
            prepare_responses_continuation(request)
        else {
            panic!("expected incremental request");
        };
        let merged = merge_responses_continuation(body, &continuation);
        let input = merged["input"].as_array().unwrap();
        assert_eq!(input.len(), 3);
        assert_eq!(input[0]["content"], "remember this");
        assert_eq!(input[1]["role"], "assistant");
        assert_eq!(
            input[2]["content"][0]["text"],
            "what did I ask you to remember?"
        );
        assert!(merged.get("previous_response_id").is_none());
    }

    #[test]
    fn expired_continuation_can_reset_only_without_tool_output() {
        let plain = json!({"previous_response_id": "resp_old", "input": "continue"});
        assert!(can_reset_responses_continuation(&plain));
        assert!(reset_responses_continuation(plain)
            .get("previous_response_id")
            .is_none());

        let tool_output = json!({
            "previous_response_id": "resp_old",
            "input": [{"type": "function_call_output", "call_id": "call_1", "output": "x"}]
        });
        assert!(!can_reset_responses_continuation(&tool_output));
    }

    #[test]
    fn store_defaults_to_true_and_is_reflected_in_stream_events() {
        assert!(responses_store_enabled(&json!({})));
        assert!(!responses_store_enabled(&json!({"store": false})));

        let mut encoder = ResponsesStreamEncoder::new_with_store("resp_1", "gpt-test", false);
        let created = encoder.start().remove(0);
        assert!(created.contains("\"store\":false"));
    }

    #[test]
    fn prepare_compact_body_preserves_trigger_and_default_instructions() {
        let body = json!({
            "model": "client-alias",
            "stream": true,
            "instructions": null,
            "input": [
                {"type": "message", "role": "user", "content": "history"},
                {"type": "compaction_trigger"}
            ]
        });
        let out = prepare_responses_compact_body(body, "gpt-5.4");
        assert_eq!(out["model"], "gpt-5.4");
        assert!(out.get("stream").is_none());
        assert_eq!(out["instructions"], "");
        let input = out["input"].as_array().unwrap();
        assert_eq!(input.len(), 2);
        assert_eq!(input[1]["type"], "compaction_trigger");
    }

    #[test]
    fn prepare_compact_body_strips_invalid_encrypted_content() {
        let body = json!({
            "model": "gpt-5.4",
            "store": false,
            "input": [
                {
                    "id": "rs_bad",
                    "type": "reasoning",
                    "encrypted_content": "not-valid",
                    "summary": []
                },
                {
                    "type": "message",
                    "role": "system",
                    "content": "hi"
                }
            ]
        });
        let out = prepare_responses_compact_body(body, "gpt-5.4");
        let input = out["input"].as_array().unwrap();
        assert!(input[0].get("encrypted_content").is_none());
        assert!(input[0].get("id").is_none());
        assert_eq!(input[1]["role"], "developer");
    }

    #[test]
    fn encode_stream_emits_responses_tool_and_terminal_events() {
        let tool_start = CanonicalChunk {
            block_index: 0,
            block_kind: Some(BlockKind::ToolUse),
            tool_use_id: Some("call_1".into()),
            tool_name: Some("search".into()),
            ..Default::default()
        };
        let tool_frame = OpenAIResponsesCodec::encode_chunk(&tool_start, "resp_1")
            .0
            .expect("tool start must emit an SSE frame");
        assert!(tool_frame.contains("response.output_item.added"));
        assert!(tool_frame.contains("call_1"));

        let terminal = CanonicalChunk {
            finish_reason: Some(FinishReason::Stop),
            usage: Some(Usage {
                prompt_tokens: 2,
                completion_tokens: 3,
                total_tokens: 5,
                ..Default::default()
            }),
            ..Default::default()
        };
        let terminal_frame = OpenAIResponsesCodec::encode_chunk(&terminal, "resp_1")
            .0
            .expect("terminal chunk must emit an SSE frame");
        assert!(terminal_frame.contains("response.completed"));
        assert!(terminal_frame.contains("\"total_tokens\":5"));
        assert!(terminal_frame.contains("\"output\":[]"));
        // Strict Responses SDKs require these nested objects on usage.
        assert!(
            terminal_frame.contains("input_tokens_details"),
            "missing input_tokens_details in: {terminal_frame}"
        );
        assert!(
            terminal_frame.contains("output_tokens_details"),
            "missing output_tokens_details in: {terminal_frame}"
        );
    }

    #[test]
    fn stateful_stream_emits_text_lifecycle_and_completed_output() {
        let mut enc = ResponsesStreamEncoder::new("resp_1", "gpt-test");
        let mut frames = enc.start();
        frames.extend(enc.push(&CanonicalChunk::text_delta("hello")));
        frames.extend(enc.push(&CanonicalChunk::finish(
            FinishReason::Stop,
            Some(Usage {
                prompt_tokens: 2,
                completion_tokens: 1,
                total_tokens: 3,
                ..Default::default()
            }),
        )));

        let payloads: Vec<Value> = frames
            .iter()
            .map(|frame| {
                serde_json::from_str(
                    frame
                        .strip_prefix("data: ")
                        .unwrap()
                        .trim_end_matches("\n\n"),
                )
                .unwrap()
            })
            .collect();
        assert!(payloads
            .iter()
            .any(|v| v["type"] == "response.output_item.added"));
        assert!(payloads
            .iter()
            .any(|v| v["type"] == "response.content_part.added"));
        let completed = payloads
            .iter()
            .find(|v| v["type"] == "response.completed")
            .expect("terminal event");
        assert_eq!(completed["response"]["output"].as_array().unwrap().len(), 1);
        assert_eq!(
            completed["response"]["output"][0]["content"][0]["text"],
            "hello"
        );
    }

    #[test]
    fn codex_chatgpt_body_forces_stream_store_and_strips_sampling() {
        let req = CanonicalChatRequest::new(
            "gpt-5.5",
            vec![
                CanonicalMessage::system("be helpful"),
                CanonicalMessage::user("hi"),
            ],
        );
        let mut req = req;
        req.sampling.temperature = Some(0.2);
        req.sampling.max_tokens = Some(64);
        let (wire, _) = OpenAIResponsesCodec::encode_request(&req, false);
        let wire = apply_codex_chatgpt_account_body(wire);
        assert_eq!(wire["stream"], true);
        assert_eq!(wire["store"], false);
        assert_eq!(wire["instructions"], "");
        assert!(wire.get("temperature").is_none());
        assert!(wire.get("max_output_tokens").is_none());
        assert_eq!(wire["input"][0]["role"], "developer");
        assert_eq!(wire["parallel_tool_calls"], true);
        assert!(wire["include"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v.as_str() == Some("reasoning.encrypted_content")));
    }

    #[test]
    fn decode_message_response() {
        let body = json!({
            "id": "resp_1",
            "model": "gpt-5",
            "status": "completed",
            "output": [{
                "type": "message",
                "role": "assistant",
                "content": [{"type": "output_text", "text": "hi there"}]
            }],
            "usage": {"input_tokens": 3, "output_tokens": 2, "total_tokens": 5}
        });
        let (resp, _) = OpenAIResponsesCodec::decode_response(body, "gpt-5").unwrap();
        assert_eq!(resp.id, "resp_1");
        assert_eq!(content_to_text(&resp.choices[0].content), "hi there");
        assert_eq!(resp.usage.total_tokens, 5);
    }

    #[test]
    fn encode_response_usage_includes_required_details() {
        let mut resp = CanonicalChatResponse {
            id: "resp_u".into(),
            request_id: "r".into(),
            model: "gpt-5".into(),
            choices: vec![CanonicalMessage::assistant("ok")],
            finish_reason: FinishReason::Stop,
            usage: Usage {
                prompt_tokens: 10,
                completion_tokens: 4,
                total_tokens: 14,
                reasoning_tokens: 2,
                cache_read_tokens: 3,
                cache_write_tokens: 1,
            },
            created_at: chrono::Utc::now(),
        };
        let wire = OpenAIResponsesCodec::encode_response(&resp);
        let usage = &wire["usage"];
        assert_eq!(usage["input_tokens"], 10);
        assert_eq!(usage["output_tokens"], 4);
        assert_eq!(usage["total_tokens"], 14);
        assert_eq!(usage["input_tokens_details"]["cached_tokens"], 3);
        assert_eq!(usage["input_tokens_details"]["cache_write_tokens"], 1);
        assert_eq!(usage["output_tokens_details"]["reasoning_tokens"], 2);

        // Even with zero details, nested objects must be present (required by SDK).
        resp.usage = Usage {
            prompt_tokens: 1,
            completion_tokens: 1,
            total_tokens: 2,
            ..Default::default()
        };
        let wire = OpenAIResponsesCodec::encode_response(&resp);
        assert!(wire["usage"]["input_tokens_details"].is_object());
        assert!(wire["usage"]["output_tokens_details"].is_object());
        assert_eq!(wire["usage"]["input_tokens_details"]["cached_tokens"], 0);
        assert_eq!(
            wire["usage"]["output_tokens_details"]["reasoning_tokens"],
            0
        );
    }

    #[test]
    fn stateful_encoder_completed_usage_has_details() {
        let mut enc = ResponsesStreamEncoder::new("resp_det", "gpt-test");
        let _ = enc.start();
        let _ = enc.push(&CanonicalChunk::text_delta("hi"));
        let frames = enc.push(&CanonicalChunk::finish(
            FinishReason::Stop,
            Some(Usage {
                prompt_tokens: 5,
                completion_tokens: 2,
                total_tokens: 7,
                reasoning_tokens: 1,
                cache_read_tokens: 0,
                cache_write_tokens: 0,
            }),
        ));
        let completed = frames
            .iter()
            .find(|f| f.contains("response.completed"))
            .expect("completed event");
        let data = completed.trim_start_matches("data: ").trim();
        let v: Value = serde_json::from_str(data).unwrap();
        let usage = &v["response"]["usage"];
        assert!(usage["input_tokens_details"].is_object());
        assert!(usage["output_tokens_details"].is_object());
        assert_eq!(usage["output_tokens_details"]["reasoning_tokens"], 1);
    }

    #[test]
    fn decode_text_delta_chunk() {
        let data = r#"{"type":"response.output_text.delta","delta":"Hello"}"#;
        let (chunks, _) = OpenAIResponsesCodec::decode_chunk(data).unwrap();
        assert_eq!(chunks.len(), 1);
        match &chunks[0].delta {
            Some(BlockDelta::TextDelta { text }) => assert_eq!(text, "Hello"),
            other => panic!("unexpected delta: {other:?}"),
        }
    }

    #[test]
    fn decode_reasoning_summary_delta() {
        let data = r#"{"type":"response.reasoning_summary_text.delta","delta":"plan"}"#;
        let (chunks, _) = OpenAIResponsesCodec::decode_chunk(data).unwrap();
        assert!(matches!(
            &chunks[0].delta,
            Some(BlockDelta::ThinkingDelta { thinking }) if thinking == "plan"
        ));
    }

    #[test]
    fn decode_function_call_added_and_args() {
        let mut st = ResponsesStreamState::default();
        let added = r#"{"type":"response.output_item.added","output_index":0,"item":{"type":"function_call","call_id":"c1","name":"search"}}"#;
        let (chunks, _) = OpenAIResponsesCodec::decode_chunk_stateful(&mut st, added).unwrap();
        assert_eq!(chunks[0].tool_use_id.as_deref(), Some("c1"));
        assert_eq!(chunks[0].tool_name.as_deref(), Some("search"));

        let args = r#"{"type":"response.function_call_arguments.delta","output_index":0,"delta":"{\"q\":1}"}"#;
        let (chunks, _) = OpenAIResponsesCodec::decode_chunk_stateful(&mut st, args).unwrap();
        assert!(matches!(
            &chunks[0].delta,
            Some(BlockDelta::InputJsonDelta { partial_json }) if partial_json.contains("q")
        ));
    }

    /// Repro Claude "Invalid tool parameters": streaming args then full JSON again
    /// produced `{"a":1}{"a":1}`.
    #[test]
    fn decode_tool_args_not_duplicated_after_deltas() {
        let mut st = ResponsesStreamState::default();
        let _ = OpenAIResponsesCodec::decode_chunk_stateful(
            &mut st,
            r#"{"type":"response.output_item.added","output_index":0,"item":{"type":"function_call","call_id":"c1","name":"Bash","arguments":""}}"#,
        )
        .unwrap();
        for part in [r#"{"#, r#""command":"pwd""#, r#"}"#] {
            let ev = format!(
                r#"{{"type":"response.function_call_arguments.delta","output_index":0,"delta":{}}}"#,
                serde_json::to_string(part).unwrap()
            );
            let (chunks, _) = OpenAIResponsesCodec::decode_chunk_stateful(&mut st, &ev).unwrap();
            assert_eq!(chunks.len(), 1);
        }
        // .done with full args must be ignored
        let (chunks, _) = OpenAIResponsesCodec::decode_chunk_stateful(
            &mut st,
            r#"{"type":"response.function_call_arguments.done","output_index":0,"arguments":"{\"command\":\"pwd\"}"}"#,
        )
        .unwrap();
        assert!(chunks.is_empty(), "done must not re-emit args: {chunks:?}");

        // output_item.done must not re-emit
        let (chunks, _) = OpenAIResponsesCodec::decode_chunk_stateful(
            &mut st,
            r#"{"type":"response.output_item.done","output_index":0,"item":{"type":"function_call","call_id":"c1","name":"Bash","arguments":"{\"command\":\"pwd\"}"}}"#,
        )
        .unwrap();
        assert!(
            chunks.is_empty(),
            "output_item.done must not re-emit: {chunks:?}"
        );

        // completed must not re-emit tool args
        let (chunks, _) = OpenAIResponsesCodec::decode_chunk_stateful(
            &mut st,
            r#"{"type":"response.completed","response":{"id":"r1","usage":{"input_tokens":1,"output_tokens":1,"total_tokens":2},"output":[{"type":"function_call","call_id":"c1","name":"Bash","arguments":"{\"command\":\"pwd\"}"}]}}"#,
        )
        .unwrap();
        assert!(
            !chunks
                .iter()
                .any(|c| matches!(&c.delta, Some(BlockDelta::InputJsonDelta { .. }))),
            "completed must not re-emit args: {chunks:?}"
        );

        // Simulate Anthropic encoder path: concat all arg deltas only once
        let mut st2 = ResponsesStreamState::default();
        let mut acc = String::new();
        let events = [
            r#"{"type":"response.output_item.added","output_index":0,"item":{"type":"function_call","call_id":"c1","name":"Bash"}}"#,
            r#"{"type":"response.function_call_arguments.delta","delta":"{\"command\":\"pwd\"}"}"#,
            r#"{"type":"response.function_call_arguments.done","arguments":"{\"command\":\"pwd\"}"}"#,
            r#"{"type":"response.completed","response":{"id":"r1","output":[{"type":"function_call","call_id":"c1","name":"Bash","arguments":"{\"command\":\"pwd\"}"}],"usage":{"input_tokens":1,"output_tokens":1,"total_tokens":2}}}"#,
        ];
        for ev in events {
            let (chunks, _) = OpenAIResponsesCodec::decode_chunk_stateful(&mut st2, ev).unwrap();
            for c in chunks {
                if let Some(BlockDelta::InputJsonDelta { partial_json }) = c.delta {
                    acc.push_str(&partial_json);
                }
            }
        }
        assert_eq!(acc, r#"{"command":"pwd"}"#);
        assert!(serde_json::from_str::<serde_json::Value>(&acc).is_ok());
    }

    /// Buffered Codex: only terminal event with full output (terra symptom).
    #[test]
    fn decode_completed_recovers_message_when_no_deltas() {
        let data = r#"{
            "type":"response.completed",
            "response":{
                "id":"resp_x",
                "usage":{"input_tokens":10,"output_tokens":5,"total_tokens":15},
                "output":[
                    {"type":"reasoning","summary":[{"type":"summary_text","text":"think hard"}]},
                    {"type":"message","content":[{"type":"output_text","text":"hello terra"}]}
                ]
            }
        }"#;
        let (chunks, _) = OpenAIResponsesCodec::decode_chunk(data).unwrap();
        assert!(
            chunks.iter().any(|c| matches!(
                &c.delta,
                Some(BlockDelta::ThinkingDelta { thinking }) if thinking == "think hard"
            )),
            "expected thinking from completed.output: {chunks:?}"
        );
        assert!(
            chunks.iter().any(|c| matches!(
                &c.delta,
                Some(BlockDelta::TextDelta { text }) if text == "hello terra"
            )),
            "expected text from completed.output: {chunks:?}"
        );
        assert!(chunks
            .iter()
            .any(|c| c.finish_reason == Some(FinishReason::Stop)));
    }

    #[test]
    fn decode_completed_skips_text_if_already_streamed() {
        let mut st = ResponsesStreamState::default();
        let delta = r#"{"type":"response.output_text.delta","delta":"hi"}"#;
        let _ = OpenAIResponsesCodec::decode_chunk_stateful(&mut st, delta).unwrap();
        let done = r#"{
            "type":"response.completed",
            "response":{
                "id":"resp_x",
                "usage":{"input_tokens":1,"output_tokens":1,"total_tokens":2},
                "output":[{"type":"message","content":[{"type":"output_text","text":"hi DUPLICATE"}]}]
            }
        }"#;
        let (chunks, _) = OpenAIResponsesCodec::decode_chunk_stateful(&mut st, done).unwrap();
        assert!(
            !chunks.iter().any(|c| matches!(
                &c.delta,
                Some(BlockDelta::TextDelta { text }) if text.contains("DUPLICATE")
            )),
            "must not re-emit terminal text after live deltas: {chunks:?}"
        );
        assert!(chunks
            .iter()
            .any(|c| c.finish_reason == Some(FinishReason::Stop)));
    }

    #[test]
    fn encode_user_tool_result_as_function_call_output() {
        let msg = CanonicalMessage {
            role: Role::User,
            content: vec![
                CanonicalContent::ToolResult {
                    tool_use_id: "c1".into(),
                    content: vec![CanonicalContent::Text { text: "ok".into() }],
                    is_error: None,
                },
                CanonicalContent::Text {
                    text: "continue".into(),
                },
            ],
            name: None,
        };
        let req = CanonicalChatRequest::new("gpt-5.6-terra", vec![msg]);
        let (wire, _) = OpenAIResponsesCodec::encode_request(&req, true);
        let input = wire["input"].as_array().unwrap();
        assert_eq!(input[0]["type"], "function_call_output");
        assert_eq!(input[0]["call_id"], "c1");
        assert_eq!(input[1]["role"], "user");
    }

    #[test]
    fn responses_p3_fields_round_trip_without_codex_strip() {
        let body = json!({
            "model": "gpt-5",
            "input": "hi",
            "background": true,
            "conversation": "conv_abc",
            "max_tool_calls": 3,
            "prompt_cache_key": "pk",
            "safety_identifier": "sid",
            "store": true
        });
        let req = OpenAIResponsesCodec::decode_request(
            body,
            "gpt-5".into(),
            false,
            "rid".into(),
            "kid".into(),
        )
        .unwrap();
        assert_eq!(req.meta.extra.get("background"), Some(&json!(true)));
        assert_eq!(
            req.meta.extra.get("conversation"),
            Some(&json!("conv_abc"))
        );
        assert_eq!(req.meta.extra.get("max_tool_calls"), Some(&json!(3)));
        let (wire, _) = OpenAIResponsesCodec::encode_request(&req, false);
        assert_eq!(wire["background"], true);
        assert_eq!(wire["conversation"], "conv_abc");
        assert_eq!(wire["max_tool_calls"], 3);
        assert_eq!(wire["prompt_cache_key"], "pk");
        assert_eq!(wire["safety_identifier"], "sid");
        // Client store=true is re-emitted on generic encode.
        assert_eq!(wire["store"], true);
    }

    #[test]
    fn codex_account_still_forces_stream_and_store_false() {
        let mut req = CanonicalChatRequest::new("gpt-5.6", vec![CanonicalMessage::user("hi")]);
        req.meta.extra.insert("background".into(), json!(true));
        req.meta.extra.insert("max_tool_calls".into(), json!(2));
        req.sampling.temperature = Some(0.5);
        let (wire, _) = OpenAIResponsesCodec::encode_request(&req, false);
        let wire = apply_codex_chatgpt_account_body(wire);
        assert_eq!(wire["stream"], true);
        assert_eq!(wire["store"], false);
        // Sampling stripped for Codex account.
        assert!(wire.get("temperature").is_none());
        // background may remain (not in Codex strip list) — that's fine.
        assert_eq!(wire["background"], true);
    }
}
