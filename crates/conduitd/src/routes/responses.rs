//! POST /v1/responses and /v1/responses/compact.

use std::sync::Arc;

use axum::{
    body::Body,
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use conduit_codec::{
    response_output_items, responses_store_enabled, OpenAIResponsesCodec, ResponsesStreamEncoder,
    WireCodec,
};
use conduit_store::ResponseContinuationRepo;
use futures::StreamExt;
use serde_json::Value;
use tracing::{debug, info, instrument, warn};
use ulid::Ulid;

use super::common::{
    extract_client_headers, extract_key_id, is_upstream_fault, sse_response, stamp_request_id,
    status_for_gateway_error, with_request_id,
};
use crate::{
    responses_adapter::{
        apply_continuation, continuation_key_scope, persist_continuation, ContinuationError,
    },
    state::DaemonState,
};

/// POST /v1/responses — OpenAI Responses API endpoint.
#[instrument(skip_all, fields(endpoint = "/v1/responses", request_id = tracing::field::Empty))]
pub async fn responses(
    State(state): State<Arc<DaemonState>>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    let key_id = extract_key_id(&headers);
    let continuation_scope = continuation_key_scope(key_id.as_deref());
    let store_continuation = responses_store_enabled(&body);
    let stream = body
        .get("stream")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let request_id = Ulid::new().to_string();
    stamp_request_id(&request_id);

    let alias = match body.get("model").and_then(|v| v.as_str()) {
        Some(model) if !model.is_empty() => model.to_string(),
        _ => {
            debug!(
                endpoint = "/v1/responses",
                request_id = %request_id,
                "reject: model field required"
            );
            return with_request_id(
                (
                    StatusCode::BAD_REQUEST,
                    Json(OpenAIResponsesCodec::error_body(
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
        endpoint = "/v1/responses",
        alias = %alias,
        stream,
        has_key = key_id.is_some(),
        store_continuation,
        request_id = %request_id,
        "gateway request accept"
    );

    let continuation_repo = ResponseContinuationRepo::new(&state.pool);
    let adapted_body = match apply_continuation(
        &continuation_repo,
        body.clone(),
        &continuation_scope,
    )
    .await
    {
        Ok(body) => body,
        Err(ContinuationError::Missing(message)) => {
            return with_request_id(
                (
                    StatusCode::BAD_REQUEST,
                    Json(OpenAIResponsesCodec::error_body(
                        "invalid_request_error",
                        None,
                        &format!(
                            "previous_response_id `{message}` is unknown or expired; resend the full input transcript"
                        ),
                    )),
                )
                    .into_response(),
                &request_id,
            );
        }
        Err(error) => {
            warn!(
                error = %error,
                request_id = %request_id,
                "responses continuation lookup failed"
            );
            return with_request_id(
                (
                    StatusCode::SERVICE_UNAVAILABLE,
                    Json(OpenAIResponsesCodec::error_body(
                        "server_error",
                        None,
                        "responses continuation store unavailable",
                    )),
                )
                    .into_response(),
                &request_id,
            );
        }
    };
    let continuation_input = adapted_body.get("input").cloned().unwrap_or(Value::Null);

    let canonical_req = match OpenAIResponsesCodec::decode_request(
        adapted_body,
        alias.clone(),
        stream,
        request_id.clone(),
        key_id.clone().unwrap_or_default(),
    ) {
        Ok(request) => request,
        Err(error) => {
            let msg = error.to_string();
            debug!(
                endpoint = "/v1/responses",
                alias = %alias,
                request_id = %request_id,
                error = %msg,
                "gateway request decode failed"
            );
            return with_request_id(
                (
                    StatusCode::BAD_REQUEST,
                    Json(OpenAIResponsesCodec::error_body(
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

    let ingress_wire = conduit_pipeline::IngressWire {
        format: conduit_ir::wire_format::WireFormat::OpenaiResponses,
    };

    match state
        .pipeline
        .run(
            canonical_req,
            key_id,
            extract_client_headers(&headers),
            ingress_wire,
        )
        .await
    {
        Ok(conduit_pipeline::handle::PipelineResult::Complete(response)) => {
            info!(
                endpoint = "/v1/responses",
                alias = %alias,
                request_id = %request_id,
                stream = false,
                "gateway response complete"
            );
            let mut response = OpenAIResponsesCodec::encode_response(&response);
            response["store"] = Value::Bool(store_continuation);
            if store_continuation {
                if let Err(error) = persist_continuation(
                    &continuation_repo,
                    response
                        .get("id")
                        .and_then(Value::as_str)
                        .unwrap_or_default(),
                    &continuation_scope,
                    continuation_input,
                    response_output_items(&response),
                )
                .await
                {
                    warn!(error = %error, "responses continuation write failed");
                }
            }
            with_request_id(Json(response).into_response(), &request_id)
        }
        Ok(conduit_pipeline::handle::PipelineResult::Streaming(stream)) => {
            info!(
                endpoint = "/v1/responses",
                alias = %alias,
                request_id = %request_id,
                stream = true,
                "gateway stream response begin"
            );
            let resp_id = format!("resp_{}", Ulid::new());
            let continuation_pool = state.pool.clone();
            let continuation_scope = continuation_scope.clone();
            let continuation_input = continuation_input.clone();
            let stream_request_id = request_id.clone();
            let sse_stream = async_stream::stream! {
                let mut encoder = ResponsesStreamEncoder::new_with_store(
                    resp_id.clone(),
                    alias,
                    store_continuation,
                );
                for frame in encoder.start() {
                    yield Ok::<_, std::convert::Infallible>(frame);
                }
                futures::pin_mut!(stream);
                while let Some(result) = stream.next().await {
                    let is_terminal = matches!(&result, Ok(chunk) if chunk.finish_reason.is_some());
                    let frames = match result {
                        Ok(chunk) => encoder.push(&chunk),
                        Err(error) => {
                            debug!(
                                endpoint = "/v1/responses",
                                request_id = %stream_request_id,
                                resp_id = %resp_id,
                                error = %error,
                                "gateway responses stream chunk error"
                            );
                            vec![OpenAIResponsesCodec::stream_error_sse(&error.to_string())]
                        }
                    };
                    if is_terminal && store_continuation {
                        let repo = ResponseContinuationRepo::new(&continuation_pool);
                        if let Err(error) = persist_continuation(
                            &repo,
                            &resp_id,
                            &continuation_scope,
                            continuation_input.clone(),
                            encoder.output_items(),
                        )
                        .await
                        {
                            warn!(error = %error, "responses continuation write failed");
                        }
                    }
                    for frame in frames {
                        yield Ok(frame);
                    }
                }
            };
            let body = Body::from_stream(sse_stream.map(
                |result: Result<String, std::convert::Infallible>| {
                    result.map(|frame| axum::body::Bytes::from(frame.into_bytes()))
                },
            ));
            sse_response(body, &request_id)
        }
        Err(error) => {
            let status = status_for_gateway_error(&error);
            if is_upstream_fault(&error) {
                warn!(
                    endpoint = "/v1/responses",
                    alias = %alias,
                    request_id = %request_id,
                    status = %status,
                    error = %error,
                    "gateway request failed (upstream)"
                );
            } else {
                debug!(
                    endpoint = "/v1/responses",
                    alias = %alias,
                    request_id = %request_id,
                    status = %status,
                    error = %error,
                    "gateway request failed"
                );
            }
            with_request_id(
                (
                    status,
                    Json(OpenAIResponsesCodec::error_body(
                        "error",
                        None,
                        &error.to_string(),
                    )),
                )
                    .into_response(),
                &request_id,
            )
        }
    }
}

/// POST /v1/responses/compact — OpenAI Responses context compaction.
///
/// CLIProxyAPI parity: non-stream only; body is forwarded (after light prep)
/// to Codex `{base}/responses/compact` so items like `compaction_trigger`
/// survive. Does not IR-round-trip.
#[instrument(
    skip_all,
    fields(endpoint = "/v1/responses/compact", request_id = tracing::field::Empty)
)]
pub async fn responses_compact(
    State(state): State<Arc<DaemonState>>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    let request_id = Ulid::new().to_string();
    stamp_request_id(&request_id);
    let key_id = extract_key_id(&headers);
    if body.get("stream").and_then(|v| v.as_bool()) == Some(true) {
        return with_request_id(
            (
                StatusCode::BAD_REQUEST,
                Json(OpenAIResponsesCodec::error_body(
                    "invalid_request_error",
                    None,
                    "Streaming not supported for compact responses",
                )),
            )
                .into_response(),
            &request_id,
        );
    }
    let alias = match body.get("model").and_then(|v| v.as_str()) {
        Some(model) if !model.is_empty() => model.to_string(),
        _ => {
            return with_request_id(
                (
                    StatusCode::BAD_REQUEST,
                    Json(OpenAIResponsesCodec::error_body(
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
        endpoint = "/v1/responses/compact",
        alias = %alias,
        has_key = key_id.is_some(),
        request_id = %request_id,
        "gateway compact request accept"
    );

    match state
        .pipeline
        .run_compact(
            alias.clone(),
            body,
            key_id,
            extract_client_headers(&headers),
            request_id.clone(),
        )
        .await
    {
        Ok(response) => {
            info!(
                endpoint = "/v1/responses/compact",
                alias = %alias,
                request_id = %request_id,
                "gateway compact complete"
            );
            with_request_id(Json(response).into_response(), &request_id)
        }
        Err(error) => {
            let status = status_for_gateway_error(&error);
            warn!(
                endpoint = "/v1/responses/compact",
                alias = %alias,
                request_id = %request_id,
                status = %status,
                error = %error,
                "gateway compact failed"
            );
            with_request_id(
                (
                    status,
                    Json(OpenAIResponsesCodec::error_body(
                        "error",
                        None,
                        &error.to_string(),
                    )),
                )
                    .into_response(),
                &request_id,
            )
        }
    }
}
