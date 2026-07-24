//! POST /v1/messages — Anthropic Messages API (native ingress).

use std::sync::Arc;

use axum::{
    body::Body,
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use conduit_codec::{anthropic::AnthropicCodec, WireCodec};
use futures::StreamExt;
use serde_json::Value;
use tracing::{debug, info, instrument};
use ulid::Ulid;

use super::common::{
    extract_client_headers, extract_key_id, fail_gateway, sse_response, stamp_request_id,
    with_request_id,
};
use crate::state::DaemonState;

/// POST /v1/messages — Anthropic Messages API (native ingress).
///
/// Shares the same pipeline as chat completions; only the wire codec differs.
#[instrument(skip_all, fields(endpoint = "/v1/messages", request_id = tracing::field::Empty))]
pub async fn messages(
    State(state): State<Arc<DaemonState>>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    let key_id = extract_key_id(&headers);
    let stream = body
        .get("stream")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let request_id = Ulid::new().to_string();
    stamp_request_id(&request_id);

    let alias = match body.get("model").and_then(|v| v.as_str()) {
        Some(a) if !a.is_empty() => a.to_string(),
        _ => {
            debug!(
                endpoint = "/v1/messages",
                request_id = %request_id,
                "reject: model field required"
            );
            return with_request_id(
                (
                    StatusCode::BAD_REQUEST,
                    Json(AnthropicCodec::error_body(
                        "invalid_request_error",
                        None,
                        "model: Field required",
                    )),
                )
                    .into_response(),
                &request_id,
            );
        }
    };

    // Anthropic Messages API requires max_tokens.
    if body.get("max_tokens").and_then(|v| v.as_u64()).is_none() {
        debug!(
            endpoint = "/v1/messages",
            alias = %alias,
            request_id = %request_id,
            "reject: max_tokens required"
        );
        return with_request_id(
            (
                StatusCode::BAD_REQUEST,
                Json(AnthropicCodec::error_body(
                    "invalid_request_error",
                    None,
                    "max_tokens: Field required",
                )),
            )
                .into_response(),
            &request_id,
        );
    }

    if body.get("messages").and_then(|v| v.as_array()).is_none() {
        debug!(
            endpoint = "/v1/messages",
            alias = %alias,
            request_id = %request_id,
            "reject: messages required"
        );
        return with_request_id(
            (
                StatusCode::BAD_REQUEST,
                Json(AnthropicCodec::error_body(
                    "invalid_request_error",
                    None,
                    "messages: Field required",
                )),
            )
                .into_response(),
            &request_id,
        );
    }

    debug!(
        endpoint = "/v1/messages",
        alias = %alias,
        stream,
        has_key = key_id.is_some(),
        request_id = %request_id,
        "gateway request accept"
    );

    let canonical_req = match AnthropicCodec::decode_request(
        body.clone(),
        alias.clone(),
        stream,
        request_id.clone(),
        key_id.clone().unwrap_or_default(),
    ) {
        Ok(r) => r,
        Err(e) => {
            let msg = e.to_string();
            debug!(
                endpoint = "/v1/messages",
                alias = %alias,
                request_id = %request_id,
                error = %msg,
                "gateway request decode failed"
            );
            return with_request_id(
                (
                    StatusCode::BAD_REQUEST,
                    Json(AnthropicCodec::error_body(
                        "invalid_request_error",
                        None,
                        &msg,
                    )),
                )
                    .into_response(),
                &request_id,
            );
        }
    };

    let client_headers = extract_client_headers(&headers);
    let ingress_wire = conduit_pipeline::IngressWire {
        format: conduit_ir::wire_format::WireFormat::AnthropicMessages,
    };

    match state
        .pipeline
        .run(canonical_req, key_id, client_headers, ingress_wire)
        .await
    {
        Ok(conduit_pipeline::handle::PipelineResult::Complete(resp)) => {
            info!(
                endpoint = "/v1/messages",
                alias = %alias,
                request_id = %request_id,
                stream = false,
                "gateway response complete"
            );
            let body = AnthropicCodec::encode_response(&resp);
            with_request_id(Json(body).into_response(), &request_id)
        }
        Ok(conduit_pipeline::handle::PipelineResult::Streaming(stream)) => {
            info!(
                endpoint = "/v1/messages",
                alias = %alias,
                request_id = %request_id,
                stream = true,
                "gateway stream response begin"
            );
            let resp_id = Ulid::new().to_string();
            let model = alias.clone();
            let stream_request_id = request_id.clone();
            // CLIProxyAPI-parity stateful Anthropic SSE lifecycle.
            // Do not pre-emit message_start with input_tokens=0 — Claude Code
            // reads context from message_start.usage.input_tokens. Anthropic
            // upstream yields a usage-only IR chunk first; the encoder stamps
            // real tokens into message_start on that chunk (or on first content).
            let sse_stream = async_stream::stream! {
                let mut encoder = conduit_codec::anthropic::stream::AnthropicStreamEncoder::new(
                    resp_id.clone(),
                    model.clone(),
                );
                let mut stream = stream;
                while let Some(result) = stream.next().await {
                    match result {
                        Ok(chunk) => {
                            for frame in encoder.push(&chunk) {
                                yield Ok::<_, std::convert::Infallible>(frame);
                            }
                        }
                        Err(e) => {
                            debug!(
                                endpoint = "/v1/messages",
                                request_id = %stream_request_id,
                                resp_id = %resp_id,
                                error = %e,
                                "gateway messages stream chunk error"
                            );
                            yield Ok(AnthropicCodec::stream_error_sse(&e.to_string()));
                        }
                    }
                }
                for frame in encoder.finish() {
                    yield Ok(frame);
                }
            };

            let body = Body::from_stream(sse_stream.map(
                |r: Result<String, std::convert::Infallible>| {
                    r.map(|s| axum::body::Bytes::from(s.into_bytes()))
                },
            ));

            sse_response(body, &request_id)
        }
        Err(e) => fail_gateway(
            "/v1/messages",
            &alias,
            &request_id,
            &e,
            AnthropicCodec::error_body,
            "api_error",
        )
    }
}
