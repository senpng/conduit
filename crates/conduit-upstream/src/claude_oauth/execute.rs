//! End-to-end Claude OAuth Messages call via Chrome-impersonating client.
//!
//! Mirrors CLIProxyAPI `ClaudeExecutor` + `NewUtlsHTTPClient(HelloChrome_Auto)`.

use std::{collections::HashMap, time::Duration};

use conduit_codec::WireCodec;
use conduit_ir::{canonical::CanonicalChatRequest, error::ProviderError};
use eventsource_stream::Eventsource;
use futures::stream::{StreamExt, TryStreamExt};
use secrecy::{ExposeSecret, SecretString};
use serde_json::{Map, Value};
use tracing::{debug, warn};

use super::{
    body::prepare_oauth_body,
    headers::build_claude_oauth_headers,
    http_client::chrome_client,
    is_claude_oauth_kind, messages_url_with_beta,
    options::ClaudeOAuthRelayOptions,
    tools::{reverse_remap_response, reverse_remap_stream_payload},
};
use crate::{
    provider::{apply_request_overrides, ChatResult, StreamResult},
    rate_limit::{self, RateLimitHeaderSink},
    sse::{classify_transport_message, with_stream_timeouts, StreamTimeoutOpts},
};

fn map_status(status: u16, body: &str) -> ProviderError {
    match status {
        401 | 403 => ProviderError::Unauthorized(body.to_string()),
        429 => ProviderError::RateLimited(body.to_string()),
        400 | 422 => ProviderError::InvalidRequest(body.to_string()),
        413 => ProviderError::ContextLengthExceeded,
        s if s >= 500 => ProviderError::Upstream5xx(format!("{s}: {body}")),
        s => ProviderError::InvalidRequest(format!("HTTP {s}: {body}")),
    }
}

/// Map a wreq (Chrome-TLS) error into `ProviderError`.
fn map_wreq_error(e: wreq::Error) -> ProviderError {
    if e.is_timeout() {
        ProviderError::Timeout
    } else {
        classify_transport_message(&e.to_string())
    }
}

fn map_eventsource_error(e: eventsource_stream::EventStreamError<ProviderError>) -> ProviderError {
    match e {
        eventsource_stream::EventStreamError::Transport(inner) => inner,
        other => classify_transport_message(&other.to_string()),
    }
}

fn messages_url(base_url: &str) -> String {
    let base = base_url.trim_end_matches('/');
    let url = if base.ends_with("/v1") {
        format!("{base}/messages")
    } else if base.contains("/v1/messages") {
        base.to_string()
    } else {
        format!("{base}/v1/messages")
    };
    messages_url_with_beta(&url)
}

fn oauth_request_headers(
    access_token: &str,
    stream: bool,
    extra_betas: &[String],
    opts: &ClaudeOAuthRelayOptions,
) -> Vec<(String, String)> {
    let mut headers = vec![("Authorization".into(), format!("Bearer {access_token}"))];
    headers.extend(build_claude_oauth_headers(
        access_token,
        stream,
        extra_betas,
        opts,
    ));
    headers
}

fn apply_headers(
    mut builder: wreq::RequestBuilder,
    headers: &[(String, String)],
) -> wreq::RequestBuilder {
    for (key, value) in headers {
        builder = builder.header(key, value);
    }
    builder
}

fn apply_upstream_model(mut body: Value, upstream_model: &str) -> Value {
    if !upstream_model.is_empty() {
        body["model"] = serde_json::json!(upstream_model);
    }
    body
}

/// Non-streaming Claude OAuth Messages call (Chrome TLS fingerprint).
///
/// `upstream_model` is the routed provider `model_id` (not the client alias).
pub async fn chat_oauth<C: WireCodec + 'static>(
    base_url: &str,
    kind: &str,
    req: &CanonicalChatRequest,
    upstream_model: &str,
    secret: &SecretString,
    opts: &ClaudeOAuthRelayOptions,
    request_overrides: &Map<String, Value>,
    overall_ms: u64,
    rate_limit_sink: Option<RateLimitHeaderSink>,
    provider_id: &str,
) -> Result<ChatResult, ProviderError> {
    debug_assert!(is_claude_oauth_kind(kind));
    let url = messages_url(base_url);
    let (body, encode_loss) = C::encode_request(req, false);
    let mut body = apply_upstream_model(body, upstream_model);
    apply_request_overrides(&mut body, request_overrides);
    let model_for_cloak = if upstream_model.is_empty() {
        req.alias.as_str()
    } else {
        upstream_model
    };
    let prepared = prepare_oauth_body(body, model_for_cloak, secret.expose_secret(), opts);

    debug!(
        provider_id,
        kind,
        url = %url,
        upstream_model,
        stream = false,
        message_count = req.messages.len(),
        body_bytes = prepared.body.to_string().len(),
        extra_betas = prepared.extra_betas.len(),
        overall_ms,
        "claude_oauth chat request"
    );

    let builder = chrome_client()
        .post(&url)
        .timeout(Duration::from_millis(overall_ms))
        .json(&prepared.body);
    let request_headers =
        oauth_request_headers(secret.expose_secret(), false, &prepared.extra_betas, opts);
    let builder = apply_headers(builder, &request_headers);

    let started = std::time::Instant::now();
    let resp = builder.send().await.map_err(|e| {
        let err = map_wreq_error(e);
        warn!(
            provider_id,
            url = %url,
            elapsed_ms = started.elapsed().as_millis() as u64,
            error = %err,
            "claude_oauth chat transport error"
        );
        err
    })?;

    // Success and error responses may carry anthropic-ratelimit-* headers.
    rate_limit::emit(
        &rate_limit_sink,
        provider_id,
        rate_limit::collect_from_http(resp.headers()),
    );

    let status = resp.status().as_u16();
    if !(200..300).contains(&status) {
        let text = resp.text().await.unwrap_or_default();
        debug!(
            provider_id,
            status,
            body_preview = %crate::provider::truncate_for_log(&text, 400),
            elapsed_ms = started.elapsed().as_millis() as u64,
            "claude_oauth chat non-success"
        );
        return Err(map_status(status, &text));
    }

    let mut val: Value = resp
        .json()
        .await
        .map_err(|e| ProviderError::Serialization(e.to_string()))?;
    val = reverse_remap_response(val, &prepared.tool_reverse_map);

    let (response, decode_loss) = C::decode_response(val, &req.alias)
        .map_err(|e| ProviderError::Serialization(e.to_string()))?;
    debug!(
        provider_id,
        status,
        elapsed_ms = started.elapsed().as_millis() as u64,
        prompt_tokens = response.usage.prompt_tokens,
        completion_tokens = response.usage.completion_tokens,
        "claude_oauth chat ok"
    );
    let mut combined = encode_loss;
    combined.merge(decode_loss);
    Ok((response, combined))
}

/// Streaming Claude OAuth Messages call (Chrome TLS + identity Accept-Encoding).
pub async fn chat_oauth_stream<C: WireCodec + 'static>(
    base_url: &str,
    kind: &str,
    req: &CanonicalChatRequest,
    upstream_model: &str,
    secret: &SecretString,
    opts: &ClaudeOAuthRelayOptions,
    request_overrides: &Map<String, Value>,
    rate_limit_sink: Option<RateLimitHeaderSink>,
    provider_id: &str,
    stream_timeouts: StreamTimeoutOpts,
) -> Result<StreamResult, ProviderError> {
    debug_assert!(is_claude_oauth_kind(kind));
    let url = messages_url(base_url);
    let (body, encode_loss) = C::encode_request(req, true);
    let mut body = apply_upstream_model(body, upstream_model);
    apply_request_overrides(&mut body, request_overrides);
    let model_for_cloak = if upstream_model.is_empty() {
        req.alias.as_str()
    } else {
        upstream_model
    };
    let prepared = prepare_oauth_body(body, model_for_cloak, secret.expose_secret(), opts);
    let tool_reverse: HashMap<String, String> = prepared.tool_reverse_map;

    debug!(
        provider_id,
        kind,
        url = %url,
        upstream_model,
        stream = true,
        message_count = req.messages.len(),
        body_bytes = prepared.body.to_string().len(),
        extra_betas = prepared.extra_betas.len(),
        stream_idle_ms = stream_timeouts.idle_ms,
        stream_overall_ms = stream_timeouts.overall_ms,
        "claude_oauth chat_stream request"
    );

    let builder = chrome_client().post(&url).json(&prepared.body);
    let request_headers =
        oauth_request_headers(secret.expose_secret(), true, &prepared.extra_betas, opts);
    let builder = apply_headers(builder, &request_headers);

    let started = std::time::Instant::now();
    let resp = builder.send().await.map_err(|e| {
        let err = map_wreq_error(e);
        warn!(
            provider_id,
            url = %url,
            elapsed_ms = started.elapsed().as_millis() as u64,
            error = %err,
            "claude_oauth chat_stream transport error"
        );
        err
    })?;

    rate_limit::emit(
        &rate_limit_sink,
        provider_id,
        rate_limit::collect_from_http(resp.headers()),
    );

    let status = resp.status().as_u16();
    if !(200..300).contains(&status) {
        let text = resp.text().await.unwrap_or_default();
        debug!(
            provider_id,
            status,
            body_preview = %crate::provider::truncate_for_log(&text, 400),
            elapsed_ms = started.elapsed().as_millis() as u64,
            "claude_oauth chat_stream non-success"
        );
        return Err(map_status(status, &text));
    }

    debug!(
        provider_id,
        status,
        elapsed_ms = started.elapsed().as_millis() as u64,
        "claude_oauth chat_stream headers ok; beginning SSE"
    );

    let byte_stream = resp
        .bytes_stream()
        .map_err(|e| classify_transport_message(&e.to_string()));

    let mut decode_state = C::StreamState::default();
    let provider_id = provider_id.to_string();
    let sse = with_stream_timeouts(
        byte_stream
            .eventsource()
            .map_err(map_eventsource_error)
            .filter_map(|result| async move {
                match result {
                    Ok(event) => {
                        if event.data.is_empty() {
                            None
                        } else if event.data == "[DONE]" {
                            Some(Ok("[DONE]".to_string()))
                        } else {
                            Some(Ok(event.data))
                        }
                    }
                    Err(e) => Some(Err(e)),
                }
            }),
        stream_timeouts,
    );

    let stream = sse
        .map(move |result| {
            let tool_reverse = tool_reverse.clone();
            match result {
                Ok(mut data) => {
                    if data.is_empty() || data == "[DONE]" {
                        return Ok(Vec::new());
                    }
                    if !tool_reverse.is_empty() {
                        if let Ok(payload) = serde_json::from_str::<Value>(&data) {
                            let restored = reverse_remap_stream_payload(payload, &tool_reverse);
                            if let Ok(s) = serde_json::to_string(&restored) {
                                data = s;
                            }
                        }
                    }
                    match C::decode_chunk_stateful(&mut decode_state, &data) {
                        Ok((chunks, _loss)) => Ok(chunks),
                        Err(e) => {
                            warn!(
                                provider_id = %provider_id,
                                error = %e,
                                data_preview = %crate::provider::truncate_for_log(&data, 200),
                                "claude_oauth codec decode error"
                            );
                            Err(ProviderError::Serialization(e.to_string()))
                        }
                    }
                }
                Err(e) => {
                    warn!(
                        provider_id = %provider_id,
                        error = %e,
                        "claude_oauth SSE event error"
                    );
                    Err(e)
                }
            }
        })
        .map(|result| match result {
            Ok(chunks) => chunks.into_iter().map(Ok).collect::<Vec<_>>(),
            Err(e) => vec![Err(e)],
        })
        .flat_map(futures::stream::iter);

    Ok((Box::pin(stream), encode_loss))
}
