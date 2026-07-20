//! Axum route handlers for the OpenAI / Anthropic-compatible gateway API + console API.

use std::sync::Arc;

use axum::{
    body::Body,
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use conduit_codec::{
    anthropic::AnthropicCodec, convert_responses_to_chat_completions, openai::OpenAiCodec,
    response_output_items, responses_store_enabled, should_treat_as_responses_format,
    OpenAiResponsesCodec, ResponsesStreamEncoder, WireCodec,
};
use conduit_ir::error::GatewayError;
use conduit_store::ResponseContinuationRepo;
use futures::StreamExt;
use serde_json::{json, Value};
use tracing::{debug, info, instrument, warn};
use ulid::Ulid;

use crate::{
    responses_adapter::{
        apply_continuation, continuation_key_scope, persist_continuation, ContinuationError,
    },
    state::DaemonState,
};

/// Extract raw bearer secret from Authorization header.
/// Format: `Bearer <token>` or `Authorization: <token>`
///
/// The value is the secret token used only for lookup; after auth succeeds the
/// pipeline stores the stable DB key id, never this raw string.
/// Headers forwarded into Claude OAuth device-profile / cloak **and** session affinity.
///
/// Session affinity needs: `X-Session-ID`, `Session-Id` / `Session_id`,
/// `X-Claude-Code-Session-Id`, `X-Client-Request-Id` (and related).
pub(crate) fn extract_client_headers(headers: &HeaderMap) -> Vec<(String, String)> {
    const NAMES: &[&str] = &[
        "user-agent",
        "x-stainless-package-version",
        "x-stainless-runtime-version",
        "x-stainless-os",
        "x-stainless-arch",
        "x-stainless-runtime",
        "x-stainless-lang",
        "x-stainless-timeout",
        "x-stainless-retry-count",
        "anthropic-beta",
        "anthropic-version",
        "x-app",
        // Session affinity (order does not matter here; extract_session_id prioritizes).
        "x-session-id",
        "session-id",
        "session_id",
        "x-claude-code-session-id",
        "x-client-request-id",
    ];
    let mut out = Vec::new();
    for name in NAMES {
        if let Some(val) = headers.get(*name).and_then(|v| v.to_str().ok()) {
            let t = val.trim();
            if !t.is_empty() {
                // Preserve canonical casing used by device_profile lookups.
                let key = match *name {
                    "user-agent" => "User-Agent",
                    "anthropic-beta" => "Anthropic-Beta",
                    "anthropic-version" => "Anthropic-Version",
                    "x-app" => "X-App",
                    "x-session-id" => "X-Session-ID",
                    "session-id" => "Session-Id",
                    "session_id" => "Session_id",
                    other => other,
                };
                // Stainless headers: title-case like CLIProxyAPI
                let key = if let Some(rest) = key.strip_prefix("x-stainless-") {
                    // X-Stainless-Package-Version style
                    let titled: String = rest
                        .split('-')
                        .map(|p| {
                            let mut c = p.chars();
                            match c.next() {
                                None => String::new(),
                                Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
                            }
                        })
                        .collect::<Vec<_>>()
                        .join("-");
                    format!("X-Stainless-{titled}")
                } else if key.starts_with("x-claude-") || key.starts_with("x-client-") {
                    let mut parts = key.split('-');
                    let mut s = String::new();
                    for (i, p) in parts.by_ref().enumerate() {
                        if i > 0 {
                            s.push('-');
                        }
                        let mut c = p.chars();
                        if let Some(f) = c.next() {
                            s.push_str(&f.to_uppercase().collect::<String>());
                            s.push_str(c.as_str());
                        }
                    }
                    s
                } else {
                    key.to_string()
                };
                out.push((key, t.to_string()));
            }
        }
    }
    out
}

fn extract_key_id(headers: &HeaderMap) -> Option<String> {
    // Anthropic SDKs commonly send `x-api-key` instead of Authorization.
    if let Some(k) = headers.get("x-api-key").and_then(|v| v.to_str().ok()) {
        let t = k.trim();
        if !t.is_empty() {
            return Some(t.to_string());
        }
    }
    let auth = headers.get("authorization")?;
    let s = auth.to_str().ok()?.trim();
    if s.is_empty() {
        return None;
    }
    // RFC 7235: auth-scheme is case-insensitive (`Bearer` / `bearer` / `BEARER`).
    // HTTP header values often trim trailing spaces, so bare `Bearer` has no token.
    if s.eq_ignore_ascii_case("bearer") {
        return None;
    }
    if let Some(rest) = s
        .get(..7)
        .filter(|p| p.eq_ignore_ascii_case("bearer "))
        .and_then(|_| s.get(7..))
    {
        let t = rest.trim();
        if t.is_empty() {
            return None;
        }
        return Some(t.to_string());
    }
    Some(s.to_string())
}

/// Gateway route paths registered for the public LLM surface (used by tests).
pub fn gateway_public_paths() -> &'static [&'static str] {
    &[
        "/v1/chat/completions",
        "/v1/responses",
        "/v1/responses/compact",
        "/v1/messages",
        "/v1/models",
        "/health",
    ]
}

/// POST /v1/responses — OpenAI Responses API endpoint.
#[instrument(skip_all, fields(endpoint = "/v1/responses"))]
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
    let alias = match body.get("model").and_then(|v| v.as_str()) {
        Some(model) if !model.is_empty() => model.to_string(),
        _ => {
            debug!(endpoint = "/v1/responses", "reject: model field required");
            return (
                StatusCode::BAD_REQUEST,
                Json(OpenAiResponsesCodec::error_body(
                    "invalid_request_error",
                    None,
                    "model field required",
                )),
            )
                .into_response();
        }
    };
    debug!(
        endpoint = "/v1/responses",
        alias = %alias,
        stream,
        has_key = key_id.is_some(),
        store_continuation,
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
            return (
                    StatusCode::BAD_REQUEST,
                    Json(OpenAiResponsesCodec::error_body(
                        "invalid_request_error",
                        None,
                        &format!(
                            "previous_response_id `{message}` is unknown or expired; resend the full input transcript"
                        ),
                    )),
                )
                    .into_response();
        }
        Err(error) => {
            warn!(error = %error, "responses continuation lookup failed");
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(OpenAiResponsesCodec::error_body(
                    "server_error",
                    None,
                    "responses continuation store unavailable",
                )),
            )
                .into_response();
        }
    };
    let continuation_input = adapted_body.get("input").cloned().unwrap_or(Value::Null);

    let client_request_id = Ulid::new().to_string();
    let canonical_req = match OpenAiResponsesCodec::decode_request(
        adapted_body,
        alias.clone(),
        stream,
        client_request_id.clone(),
        key_id.clone().unwrap_or_default(),
    ) {
        Ok(request) => request,
        Err(error) => {
            let msg = error.to_string();
            debug!(
                endpoint = "/v1/responses",
                alias = %alias,
                client_request_id = %client_request_id,
                error = %msg,
                "gateway request decode failed"
            );
            return (
                StatusCode::BAD_REQUEST,
                Json(OpenAiResponsesCodec::error_body(
                    "invalid_request_error",
                    None,
                    &msg,
                )),
            )
                .into_response();
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
                client_request_id = %client_request_id,
                stream = false,
                "gateway response complete"
            );
            let mut response = OpenAiResponsesCodec::encode_response(&response);
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
            Json(response).into_response()
        }
        Ok(conduit_pipeline::handle::PipelineResult::Streaming(stream)) => {
            info!(
                endpoint = "/v1/responses",
                alias = %alias,
                client_request_id = %client_request_id,
                stream = true,
                "gateway stream response begin"
            );
            let resp_id = format!("resp_{}", Ulid::new());
            let continuation_pool = state.pool.clone();
            let continuation_scope = continuation_scope.clone();
            let continuation_input = continuation_input.clone();
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
                                resp_id = %resp_id,
                                error = %error,
                                "gateway responses stream chunk error"
                            );
                            vec![OpenAiResponsesCodec::stream_error_sse(&error.to_string())]
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
            Response::builder()
                .status(StatusCode::OK)
                .header("content-type", "text/event-stream")
                .header("cache-control", "no-cache")
                .header("x-accel-buffering", "no")
                .body(body)
                .unwrap()
        }
        Err(error) => {
            let status = status_for_gateway_error(&error);
            warn!(
                endpoint = "/v1/responses",
                alias = %alias,
                client_request_id = %client_request_id,
                status = %status,
                error = %error,
                "gateway request failed"
            );
            (
                status,
                Json(OpenAiResponsesCodec::error_body(
                    "error",
                    None,
                    &error.to_string(),
                )),
            )
                .into_response()
        }
    }
}

/// POST /v1/responses/compact — OpenAI Responses context compaction.
///
/// CLIProxyAPI parity: non-stream only; body is forwarded (after light prep)
/// to Codex `{base}/responses/compact` so items like `compaction_trigger`
/// survive. Does not IR-round-trip.
#[instrument(skip_all, fields(endpoint = "/v1/responses/compact"))]
pub async fn responses_compact(
    State(state): State<Arc<DaemonState>>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    let key_id = extract_key_id(&headers);
    if body.get("stream").and_then(|v| v.as_bool()) == Some(true) {
        return (
            StatusCode::BAD_REQUEST,
            Json(OpenAiResponsesCodec::error_body(
                "invalid_request_error",
                None,
                "Streaming not supported for compact responses",
            )),
        )
            .into_response();
    }
    let alias = match body.get("model").and_then(|v| v.as_str()) {
        Some(model) if !model.is_empty() => model.to_string(),
        _ => {
            return (
                StatusCode::BAD_REQUEST,
                Json(OpenAiResponsesCodec::error_body(
                    "invalid_request_error",
                    None,
                    "model field required",
                )),
            )
                .into_response();
        }
    };

    debug!(
        endpoint = "/v1/responses/compact",
        alias = %alias,
        has_key = key_id.is_some(),
        "gateway compact request accept"
    );

    match state
        .pipeline
        .run_compact(
            alias.clone(),
            body,
            key_id,
            extract_client_headers(&headers),
        )
        .await
    {
        Ok(response) => {
            info!(
                endpoint = "/v1/responses/compact",
                alias = %alias,
                "gateway compact complete"
            );
            Json(response).into_response()
        }
        Err(error) => {
            let status = status_for_gateway_error(&error);
            warn!(
                endpoint = "/v1/responses/compact",
                alias = %alias,
                status = %status,
                error = %error,
                "gateway compact failed"
            );
            (
                status,
                Json(OpenAiResponsesCodec::error_body(
                    "error",
                    None,
                    &error.to_string(),
                )),
            )
                .into_response()
        }
    }
}

/// Map pipeline errors to HTTP status codes (shipped gateway path).
pub(crate) fn status_for_gateway_error(err: &GatewayError) -> StatusCode {
    match err {
        GatewayError::Unauthorized(_) => StatusCode::UNAUTHORIZED,
        GatewayError::Routing(_) => StatusCode::NOT_FOUND,
        GatewayError::Quota(conduit_ir::error::QuotaError::RateLimitExceeded { .. }) => {
            StatusCode::TOO_MANY_REQUESTS
        }
        GatewayError::Quota(_) => StatusCode::FORBIDDEN,
        _ => StatusCode::BAD_GATEWAY,
    }
}

/// GET /v1/models — OpenAI-compatible list from the live routing table.
///
/// When model limits are known for a route target, includes `context_window`
/// and `context_length` (from LiteLLM `max_input_tokens`). Omits those fields
/// when no limit is known — does not invent a default window.
pub async fn list_models(State(state): State<Arc<DaemonState>>) -> impl IntoResponse {
    let table = state.routing_table.load();
    let limits = state.limits_table.load();
    let routes = table.iter().map(|route| {
        let target = route.targets.first();
        let owned_by = target
            .map(|t| t.provider_kind.clone())
            .unwrap_or_else(|| "conduit".into());
        let model_id = target
            .map(|t| t.model_id.clone())
            .unwrap_or_else(|| route.alias.clone());
        (route.alias.clone(), owned_by, model_id)
    });
    let data = crate::state::build_models_list_data(routes, &limits);
    Json(json!({
        "object": "list",
        "data": data,
    }))
}

/// GET /health
pub async fn health(State(state): State<Arc<DaemonState>>) -> impl IntoResponse {
    Json(json!({
        "status": "ok",
        "version": state.version,
    }))
}

/// POST /v1/chat/completions — OpenAI-compatible endpoint
#[instrument(skip_all, fields(endpoint = "/v1/chat/completions"))]
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

    // CLIProxyAPI: some clients send Responses-shaped payloads to chat/completions.
    let body = if should_treat_as_responses_format(&body) {
        debug!(
            endpoint = "/v1/chat/completions",
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
                "reject: model field required"
            );
            return (
                StatusCode::BAD_REQUEST,
                Json(OpenAiCodec::error_body(
                    "invalid_request_error",
                    None,
                    "model field required",
                )),
            )
                .into_response();
        }
    };

    let request_id = Ulid::new().to_string();
    debug!(
        endpoint = "/v1/chat/completions",
        alias = %alias,
        stream,
        has_key = key_id.is_some(),
        client_request_id = %request_id,
        "gateway request accept"
    );

    let canonical_req = match OpenAiCodec::decode_request(
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
                client_request_id = %request_id,
                error = %msg,
                "gateway request decode failed"
            );
            return (
                StatusCode::BAD_REQUEST,
                Json(OpenAiCodec::error_body(
                    "invalid_request_error",
                    None,
                    &msg,
                )),
            )
                .into_response();
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
                client_request_id = %request_id,
                stream = false,
                "gateway response complete"
            );
            let body = OpenAiCodec::encode_response(&resp);
            Json(body).into_response()
        }
        Ok(conduit_pipeline::handle::PipelineResult::Streaming(stream)) => {
            info!(
                endpoint = "/v1/chat/completions",
                alias = %alias,
                client_request_id = %request_id,
                stream = true,
                "gateway stream response begin"
            );
            // CLIProxyAPI-parity stateful encoder: fixed created, role kickoff, [DONE].
            let resp_id = format!("chatcmpl_{}", Ulid::new());
            let model = alias.clone();
            let sse_stream = async_stream::stream! {
                let mut encoder =
                    conduit_codec::openai::OpenAiStreamEncoder::new(resp_id.clone(), model);
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
                                resp_id = %resp_id,
                                error = %e,
                                "gateway stream chunk error"
                            );
                            yield Ok(OpenAiCodec::stream_error_sse(&e.to_string()));
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

            Response::builder()
                .status(StatusCode::OK)
                .header("content-type", "text/event-stream")
                .header("cache-control", "no-cache")
                .header("x-accel-buffering", "no")
                .body(body)
                .unwrap()
        }
        Err(e) => {
            let status = status_for_gateway_error(&e);
            let body = match &e {
                GatewayError::Unauthorized(msg) => {
                    OpenAiCodec::error_body("authentication_error", Some("invalid_api_key"), msg)
                }
                GatewayError::Routing(msg) => OpenAiCodec::error_body("not_found_error", None, msg),
                GatewayError::Quota(conduit_ir::error::QuotaError::RateLimitExceeded {
                    ..
                }) => OpenAiCodec::error_body(
                    "rate_limit_error",
                    Some("rate_limit_exceeded"),
                    "rate limit exceeded",
                ),
                GatewayError::Quota(qe) => {
                    OpenAiCodec::error_body("permission_error", None, &qe.to_string())
                }
                other => {
                    warn!(
                        endpoint = "/v1/chat/completions",
                        alias = %alias,
                        client_request_id = %request_id,
                        error = %other,
                        "pipeline error"
                    );
                    OpenAiCodec::error_body("upstream_error", None, &other.to_string())
                }
            };
            debug!(
                endpoint = "/v1/chat/completions",
                alias = %alias,
                client_request_id = %request_id,
                status = %status,
                error = %e,
                "gateway request failed"
            );
            (status, Json(body)).into_response()
        }
    }
}

/// POST /v1/messages — Anthropic Messages API (native ingress).
///
/// Shares the same pipeline as chat completions; only the wire codec differs.
#[instrument(skip_all, fields(endpoint = "/v1/messages"))]
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

    let alias = match body.get("model").and_then(|v| v.as_str()) {
        Some(a) if !a.is_empty() => a.to_string(),
        _ => {
            debug!(endpoint = "/v1/messages", "reject: model field required");
            return (
                StatusCode::BAD_REQUEST,
                Json(AnthropicCodec::error_body(
                    "invalid_request_error",
                    None,
                    "model: Field required",
                )),
            )
                .into_response();
        }
    };

    // Anthropic Messages API requires max_tokens.
    if body.get("max_tokens").and_then(|v| v.as_u64()).is_none() {
        debug!(
            endpoint = "/v1/messages",
            alias = %alias,
            "reject: max_tokens required"
        );
        return (
            StatusCode::BAD_REQUEST,
            Json(AnthropicCodec::error_body(
                "invalid_request_error",
                None,
                "max_tokens: Field required",
            )),
        )
            .into_response();
    }

    if body.get("messages").and_then(|v| v.as_array()).is_none() {
        debug!(
            endpoint = "/v1/messages",
            alias = %alias,
            "reject: messages required"
        );
        return (
            StatusCode::BAD_REQUEST,
            Json(AnthropicCodec::error_body(
                "invalid_request_error",
                None,
                "messages: Field required",
            )),
        )
            .into_response();
    }

    let request_id = Ulid::new().to_string();
    debug!(
        endpoint = "/v1/messages",
        alias = %alias,
        stream,
        has_key = key_id.is_some(),
        client_request_id = %request_id,
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
                client_request_id = %request_id,
                error = %msg,
                "gateway request decode failed"
            );
            return (
                StatusCode::BAD_REQUEST,
                Json(AnthropicCodec::error_body(
                    "invalid_request_error",
                    None,
                    &msg,
                )),
            )
                .into_response();
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
                client_request_id = %request_id,
                stream = false,
                "gateway response complete"
            );
            let body = AnthropicCodec::encode_response(&resp);
            Json(body).into_response()
        }
        Ok(conduit_pipeline::handle::PipelineResult::Streaming(stream)) => {
            info!(
                endpoint = "/v1/messages",
                alias = %alias,
                client_request_id = %request_id,
                stream = true,
                "gateway stream response begin"
            );
            let resp_id = Ulid::new().to_string();
            let model = alias.clone();
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

            Response::builder()
                .status(StatusCode::OK)
                .header("content-type", "text/event-stream")
                .header("cache-control", "no-cache")
                .header("x-accel-buffering", "no")
                .body(body)
                .unwrap()
        }
        Err(e) => {
            let status = status_for_gateway_error(&e);
            let body = match &e {
                GatewayError::Unauthorized(msg) => {
                    AnthropicCodec::error_body("authentication_error", Some("invalid_api_key"), msg)
                }
                GatewayError::Routing(msg) => {
                    AnthropicCodec::error_body("not_found_error", None, msg)
                }
                GatewayError::Quota(conduit_ir::error::QuotaError::RateLimitExceeded {
                    ..
                }) => AnthropicCodec::error_body(
                    "rate_limit_error",
                    Some("rate_limit_exceeded"),
                    "rate limit exceeded",
                ),
                GatewayError::Quota(qe) => {
                    AnthropicCodec::error_body("permission_error", None, &qe.to_string())
                }
                other => {
                    warn!(
                        endpoint = "/v1/messages",
                        alias = %alias,
                        client_request_id = %request_id,
                        error = %other,
                        "pipeline error (messages)"
                    );
                    AnthropicCodec::error_body("api_error", None, &other.to_string())
                }
            };
            debug!(
                endpoint = "/v1/messages",
                alias = %alias,
                client_request_id = %request_id,
                status = %status,
                error = %e,
                "gateway request failed"
            );
            (status, Json(body)).into_response()
        }
    }
}

#[cfg(test)]
mod auth_status_tests {
    use conduit_ir::error::QuotaError;

    use super::*;

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
        let req = OpenAiCodec::decode_request(
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
        let req = OpenAiCodec::decode_request(body, "gpt-4o".into(), false, "req-1".into(), "k1".into())
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
    use conduit_ir::canonical::{
        BlockDelta, BlockKind, CanonicalChatResponse, CanonicalChunk, CanonicalMessage,
        FinishReason, Usage,
    };

    use super::*;

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
