//! POST /v1/chat/completions

use std::sync::Arc;

use axum::{
    body::Body,
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use conduit_codec::{
    convert_responses_to_chat_completions, openai::OpenAICodec, should_treat_as_responses_format,
    WireCodec,
};
use futures::StreamExt;
use serde_json::Value;
use tracing::{debug, info, instrument};
use ulid::Ulid;

use super::common::{
    extract_client_headers, extract_key_id, fail_gateway, sse_response, stamp_request_id,
    with_request_id,
};
use crate::state::DaemonState;

/// POST /v1/chat/completions — OpenAI-compatible endpoint
#[instrument(
    skip_all,
    fields(endpoint = "/v1/chat/completions", request_id = tracing::field::Empty)
)]
pub async fn chat_completions(
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

    // CLIProxyAPI: some clients send Responses-shaped payloads to chat/completions.
    let body = if should_treat_as_responses_format(&body) {
        debug!(
            endpoint = "/v1/chat/completions",
            request_id = %request_id,
            "responses-shaped body detected; converting to chat completions"
        );
        convert_responses_to_chat_completions(&body, stream)
    } else {
        body
    };

    let stream = body
        .get("stream")
        .and_then(|v| v.as_bool())
        .unwrap_or(stream);
    let alias = match body.get("model").and_then(|v| v.as_str()) {
        Some(a) if !a.is_empty() => a.to_string(),
        _ => {
            debug!(
                endpoint = "/v1/chat/completions",
                request_id = %request_id,
                "reject: model field required"
            );
            return with_request_id(
                (
                    StatusCode::BAD_REQUEST,
                    Json(OpenAICodec::error_body(
                        "invalid_request_error",
                        None,
                        "model field required",
                    )),
                )
                    .into_response(),
                &request_id,
            );
        }
    };

    debug!(
        endpoint = "/v1/chat/completions",
        alias = %alias,
        stream,
        has_key = key_id.is_some(),
        request_id = %request_id,
        "gateway request accept"
    );

    let canonical_req = match OpenAICodec::decode_request(
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
                endpoint = "/v1/chat/completions",
                alias = %alias,
                request_id = %request_id,
                error = %msg,
                "gateway request decode failed"
            );
            return with_request_id(
                (
                    StatusCode::BAD_REQUEST,
                    Json(OpenAICodec::error_body(
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
        format: conduit_ir::wire_format::WireFormat::OpenaiChat,
    };

    // Shared pipeline handle — routing table is ArcSwap-loaded inside run().
    match state
        .pipeline
        .run(canonical_req, key_id, client_headers, ingress_wire)
        .await
    {
        Ok(conduit_pipeline::handle::PipelineResult::Complete(resp)) => {
            info!(
                endpoint = "/v1/chat/completions",
                alias = %alias,
                request_id = %request_id,
                stream = false,
                "gateway response complete"
            );
            let body = OpenAICodec::encode_response(&resp);
            with_request_id(Json(body).into_response(), &request_id)
        }
        Ok(conduit_pipeline::handle::PipelineResult::Streaming(stream)) => {
            info!(
                endpoint = "/v1/chat/completions",
                alias = %alias,
                request_id = %request_id,
                stream = true,
                "gateway stream response begin"
            );
            // CLIProxyAPI-parity stateful encoder: fixed created, role kickoff, [DONE].
            let resp_id = format!("chatcmpl_{}", Ulid::new());
            let model = alias.clone();
            let stream_request_id = request_id.clone();
            let sse_stream = async_stream::stream! {
                let mut encoder =
                    conduit_codec::openai::OpenAIStreamEncoder::new(resp_id.clone(), model);
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
                                endpoint = "/v1/chat/completions",
                                request_id = %stream_request_id,
                                resp_id = %resp_id,
                                error = %e,
                                "gateway stream chunk error"
                            );
                            yield Ok(OpenAICodec::stream_error_sse(&e.to_string()));
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
            "/v1/chat/completions",
            &alias,
            &request_id,
            &e,
            OpenAICodec::error_body,
            "upstream_error",
        )
    }
}
