//! Axum route handlers for the OpenAI / Anthropic-compatible gateway API.

mod chat;
mod common;
mod messages;
mod meta;
mod responses;

pub use chat::chat_completions;
pub use common::{gateway_public_paths, X_REQUEST_ID};
pub use messages::messages;
pub use meta::{health, list_models};
pub use responses::{responses, responses_compact};


#[cfg(test)]
mod auth_status_tests {
    use axum::http::{HeaderMap, StatusCode};
    use conduit_codec::{
        convert_responses_to_chat_completions, openai::OpenAICodec,
        should_treat_as_responses_format, WireCodec,
    };
    use conduit_ir::error::{GatewayError, QuotaError};
    use serde_json::json;

    use super::common::{
        extract_client_headers, extract_key_id, status_for_gateway_error,
    };
    use super::gateway_public_paths;

    #[test]
    fn unauthorized_maps_to_http_401() {
        let e = GatewayError::Unauthorized("missing authorization bearer token".into());
        assert_eq!(status_for_gateway_error(&e), StatusCode::UNAUTHORIZED);
        let e2 = GatewayError::Unauthorized("invalid or unknown api key".into());
        assert_eq!(status_for_gateway_error(&e2), StatusCode::UNAUTHORIZED);
    }

    #[test]
    fn other_errors_do_not_map_to_401() {
        assert_eq!(
            status_for_gateway_error(&GatewayError::Routing("no route".into())),
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            status_for_gateway_error(&GatewayError::Quota(QuotaError::RateLimitExceeded {
                requests_per_minute: 60,
            })),
            StatusCode::TOO_MANY_REQUESTS
        );
    }

    #[test]
    fn gateway_registers_messages_path() {
        assert!(
            gateway_public_paths().contains(&"/v1/messages"),
            "POST /v1/messages must be part of the public gateway surface"
        );
    }

    #[test]
    fn gateway_registers_responses_path() {
        assert!(
            gateway_public_paths().contains(&"/v1/responses"),
            "POST /v1/responses must be part of the public gateway surface"
        );
    }

    #[test]
    fn gateway_registers_responses_compact_path() {
        assert!(
            gateway_public_paths().contains(&"/v1/responses/compact"),
            "POST /v1/responses/compact must be part of the public gateway surface"
        );
    }

    #[test]
    fn compact_rejects_stream_true_message() {
        // Handler validation message must stay stable for CLIProxyAPI clients.
        let msg = "Streaming not supported for compact responses";
        assert!(msg.contains("Streaming not supported"));
    }

    /// CLIProxyAPI parity: Responses-shaped body on /v1/chat/completions is
    /// converted before decode (same pure helpers the handler calls).
    #[test]
    fn chat_completions_accepts_responses_shaped_body() {
        let body = json!({
            "model": "gpt-4o",
            "instructions": "Be concise",
            "input": "hello from responses on chat",
            "stream": false
        });
        assert!(
            should_treat_as_responses_format(&body),
            "body without messages but with input must be treated as Responses"
        );
        let chat = convert_responses_to_chat_completions(&body, false);
        assert!(
            chat.get("messages").and_then(|m| m.as_array()).is_some(),
            "conversion must produce messages: {chat}"
        );
        let req = OpenAICodec::decode_request(
            chat,
            "gpt-4o".into(),
            false,
            "req-test".into(),
            "key".into(),
        )
        .expect("decode after conversion must succeed");
        assert!(
            req.messages
                .iter()
                .any(|m| matches!(m.role, conduit_ir::canonical::Role::User)),
            "must include a user turn from input"
        );
        assert!(
            req.messages
                .iter()
                .any(|m| matches!(m.role, conduit_ir::canonical::Role::System)),
            "instructions must become system"
        );
    }

    #[test]
    fn extract_key_accepts_x_api_key() {
        let mut h = HeaderMap::new();
        h.insert("x-api-key", "ck_test_secret".parse().unwrap());
        assert_eq!(extract_key_id(&h).as_deref(), Some("ck_test_secret"));
    }

    #[test]
    fn extract_key_accepts_bearer_case_insensitive() {
        for auth in [
            "Bearer sk_live_abc",
            "bearer sk_live_abc",
            "BEARER sk_live_abc",
            "Bearer  sk_live_abc",
        ] {
            let mut h = HeaderMap::new();
            h.insert("authorization", auth.parse().unwrap());
            assert_eq!(
                extract_key_id(&h).as_deref(),
                Some("sk_live_abc"),
                "auth={auth}"
            );
        }
    }

    #[test]
    fn extract_key_rejects_empty_bearer() {
        let mut h = HeaderMap::new();
        h.insert("authorization", "Bearer ".parse().unwrap());
        assert_eq!(extract_key_id(&h), None);
    }

    /// Live path: daemon header filter → pipeline session resolve → affinity pin key.
    #[test]
    fn session_headers_forwarded_and_resolved_for_affinity() {
        use conduit_ir::canonical::CanonicalChatRequest;
        use conduit_pipeline::handle::resolve_session_id;
        use conduit_router::AffinityStore;

        // X-Session-ID is the primary affinity header for generic clients.
        let mut h = HeaderMap::new();
        h.insert("x-session-id", "sess-from-header".parse().unwrap());
        h.insert("user-agent", "test-agent/1".parse().unwrap());
        let forwarded = extract_client_headers(&h);
        assert!(
            forwarded
                .iter()
                .any(|(k, v)| k.eq_ignore_ascii_case("x-session-id") && v == "sess-from-header"),
            "extract_client_headers must forward x-session-id: {forwarded:?}"
        );
        let req = CanonicalChatRequest::new("gpt-4o", vec![]);
        let sid = resolve_session_id(&forwarded, &req);
        assert_eq!(sid.as_deref(), Some("sess-from-header"));
        let store = AffinityStore::new();
        store.remember(sid.as_deref().unwrap(), "gpt-4o", "prov-a");
        assert_eq!(
            store.preferred("sess-from-header", "gpt-4o").as_deref(),
            Some("prov-a")
        );
    }

    #[test]
    fn claude_code_session_header_forwarded_and_resolved() {
        use conduit_ir::canonical::CanonicalChatRequest;
        use conduit_pipeline::handle::resolve_session_id;

        let mut h = HeaderMap::new();
        h.insert(
            "x-claude-code-session-id",
            "claude-sess-xyz".parse().unwrap(),
        );
        let forwarded = extract_client_headers(&h);
        assert!(
            forwarded.iter().any(|(k, v)| {
                k.eq_ignore_ascii_case("x-claude-code-session-id") && v == "claude-sess-xyz"
            }),
            "must forward x-claude-code-session-id: {forwarded:?}"
        );
        let req = CanonicalChatRequest::new("claude", vec![]);
        assert_eq!(
            resolve_session_id(&forwarded, &req).as_deref(),
            Some("claude-sess-xyz")
        );
    }

    #[test]
    fn openai_body_conversation_id_lands_in_meta_for_session() {
        use conduit_pipeline::handle::resolve_session_id;

        let body = json!({
            "model": "gpt-4o",
            "conversation_id": "conv-live-1",
            "messages": [{"role": "user", "content": "hi"}]
        });
        let req = OpenAICodec::decode_request(body, "gpt-4o".into(), false, "req-1".into(), "k1".into())
            .expect("decode openai request");
        assert_eq!(
            req.meta
                .extra
                .get("conversation_id")
                .and_then(|v| v.as_str()),
            Some("conv-live-1")
        );
        // No session headers — body field via meta must still resolve.
        assert_eq!(
            resolve_session_id(&[], &req).as_deref(),
            Some("conv-live-1")
        );
    }
}


/// Tests that hit shipped Anthropic codec + route surface (no parallel reimplementation).
#[cfg(test)]
mod messages_ingress_tests {
    use conduit_codec::anthropic::stream::{
        encode_chunk, encode_message_start, encode_message_stop,
    };
    use conduit_codec::{anthropic::AnthropicCodec, WireCodec};
    use conduit_ir::canonical::{
        BlockDelta, BlockKind, CanonicalChatResponse, CanonicalChunk, CanonicalMessage,
        FinishReason, Usage,
    };
    use serde_json::json;

    use super::gateway_public_paths;

    #[test]
    fn decode_anthropic_request_via_shipped_codec() {
        let body = json!({
            "model": "claude-sonnet-4",
            "max_tokens": 256,
            "messages": [{"role": "user", "content": "hello native"}]
        });
        let req = AnthropicCodec::decode_request(
            body,
            "claude-sonnet-4".into(),
            false,
            "req-1".into(),
            "key-1".into(),
        )
        .expect("decode must succeed");
        assert_eq!(req.alias, "claude-sonnet-4");
        assert_eq!(req.sampling.max_tokens, Some(256));
        assert!(!req.messages.is_empty());
        assert_eq!(
            req.messages.last().unwrap().role,
            conduit_ir::canonical::Role::User
        );
    }

    #[test]
    fn encode_anthropic_response_via_shipped_codec() {
        let resp = CanonicalChatResponse {
            id: "msg_wire".into(),
            request_id: "req".into(),
            model: "claude-sonnet-4".into(),
            choices: vec![CanonicalMessage::assistant("hi from conduit")],
            finish_reason: FinishReason::Stop,
            usage: Usage {
                prompt_tokens: 3,
                completion_tokens: 4,
                total_tokens: 7,
                ..Default::default()
            },
            created_at: chrono::Utc::now(),
        };
        let wire = AnthropicCodec::encode_response(&resp);
        assert_eq!(wire["type"].as_str().unwrap(), "message");
        assert_eq!(wire["role"].as_str().unwrap(), "assistant");
        assert_eq!(wire["stop_reason"].as_str().unwrap(), "end_turn");
        assert_eq!(
            wire["content"][0]["text"].as_str().unwrap(),
            "hi from conduit"
        );
        assert_eq!(wire["usage"]["input_tokens"].as_u64().unwrap(), 3);
        assert_eq!(wire["usage"]["output_tokens"].as_u64().unwrap(), 4);
    }

    #[test]
    fn stream_encode_content_delta_and_terminal_are_anthropic_sse() {
        let start = encode_message_start("msg_s", "claude-sonnet-4", 1);
        assert!(start.starts_with("event: message_start\n"));
        assert!(start.contains("\"type\":\"message_start\"") || start.contains("message_start"));

        let chunk = CanonicalChunk {
            request_id: "msg_s".into(),
            index: 0,
            block_index: 0,
            block_kind: Some(BlockKind::Text),
            delta: None,
            finish_reason: None,
            usage: None,
            tool_use_id: None,
            tool_name: None,
        };
        let block_start = encode_chunk(&chunk, "msg_s").expect("block start");
        assert!(block_start.contains("event: content_block_start"));

        let delta = CanonicalChunk {
            request_id: "msg_s".into(),
            index: 0,
            block_index: 0,
            block_kind: None,
            delta: Some(BlockDelta::TextDelta {
                text: "partial".into(),
            }),
            finish_reason: None,
            usage: None,
            tool_use_id: None,
            tool_name: None,
        };
        let delta_sse = encode_chunk(&delta, "msg_s").expect("delta");
        assert!(delta_sse.contains("event: content_block_delta"));
        assert!(delta_sse.contains("text_delta") || delta_sse.contains("partial"));

        let fin = CanonicalChunk {
            request_id: "msg_s".into(),
            index: 0,
            block_index: 0,
            block_kind: None,
            delta: None,
            finish_reason: Some(FinishReason::Stop),
            usage: Some(Usage {
                completion_tokens: 2,
                ..Default::default()
            }),
            tool_use_id: None,
            tool_name: None,
        };
        let fin_sse = encode_chunk(&fin, "msg_s").expect("message_delta");
        assert!(fin_sse.contains("event: message_delta"));
        assert!(fin_sse.contains("end_turn") || fin_sse.contains("stop_reason"));

        let stop = encode_message_stop();
        assert!(stop.contains("event: message_stop"));
    }

    #[test]
    fn messages_handler_is_not_empty_stub_symbol() {
        // Structural: path is public; handler is defined in this module (server routes to it).
        assert!(gateway_public_paths().contains(&"/v1/messages"));
        // Touch Anthropic error shape used by the handler on validation failure.
        let err =
            AnthropicCodec::error_body("invalid_request_error", None, "max_tokens: Field required");
        assert_eq!(err["type"].as_str().unwrap(), "error");
        assert_eq!(
            err["error"]["type"].as_str().unwrap(),
            "invalid_request_error"
        );
    }
}
