//! End-to-end Claude OAuth Messages call via Chrome-impersonating client.
//!
//! Mirrors CLIProxyAPI `ClaudeExecutor` + `NewUtlsHTTPClient(HelloChrome_Auto)`.

use std::{collections::HashMap, time::Duration};

use conduit_codec::WireCodec;
use conduit_ir::{
    canonical::{CanonicalChatRequest, CanonicalChatResponse, CanonicalChunk},
    error::ProviderError,
    loss::LossReport,
};
use futures::stream::{BoxStream, StreamExt, TryStreamExt};
use secrecy::{ExposeSecret, SecretString};
use serde_json::Value;
use tracing::warn;

use super::{
    body::prepare_oauth_body,
    headers::build_claude_oauth_headers,
    http_client::chrome_client,
    is_claude_oauth_kind, messages_url_with_beta,
    options::ClaudeOAuthRelayOptions,
    tools::{reverse_remap_response, reverse_remap_stream_payload},
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

fn apply_headers(
    mut builder: wreq::RequestBuilder,
    access_token: &str,
    stream: bool,
    extra_betas: &[String],
    opts: &ClaudeOAuthRelayOptions,
) -> wreq::RequestBuilder {
    builder = builder.header("Authorization", format!("Bearer {access_token}"));
    for (k, v) in build_claude_oauth_headers(access_token, stream, extra_betas, opts) {
        builder = builder.header(k, v);
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
    overall_ms: u64,
) -> Result<(CanonicalChatResponse, LossReport), ProviderError> {
    debug_assert!(is_claude_oauth_kind(kind));
    let url = messages_url(base_url);
    let (body, encode_loss) = C::encode_request(req, false);
    let body = apply_upstream_model(body, upstream_model);
    let model_for_cloak = if upstream_model.is_empty() {
        req.alias.as_str()
    } else {
        upstream_model
    };
    let prepared = prepare_oauth_body(body, model_for_cloak, secret.expose_secret(), opts);

    let builder = chrome_client()
        .post(&url)
        .timeout(Duration::from_millis(overall_ms))
        .json(&prepared.body);
    let builder = apply_headers(
        builder,
        secret.expose_secret(),
        false,
        &prepared.extra_betas,
        opts,
    );

    let resp = builder.send().await.map_err(|e| {
        if e.is_timeout() {
            ProviderError::Timeout
        } else {
            ProviderError::Network(e.to_string())
        }
    })?;

    let status = resp.status().as_u16();
    if !(200..300).contains(&status) {
        let text = resp.text().await.unwrap_or_default();
        return Err(map_status(status, &text));
    }

    let mut val: Value = resp
        .json()
        .await
        .map_err(|e| ProviderError::Serialization(e.to_string()))?;
    val = reverse_remap_response(val, &prepared.tool_reverse_map);

    let (response, decode_loss) = C::decode_response(val, &req.alias)
        .map_err(|e| ProviderError::Serialization(e.to_string()))?;
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
    overall_ms: u64,
) -> Result<
    (
        BoxStream<'static, Result<CanonicalChunk, ProviderError>>,
        LossReport,
    ),
    ProviderError,
> {
    debug_assert!(is_claude_oauth_kind(kind));
    let url = messages_url(base_url);
    let (body, encode_loss) = C::encode_request(req, true);
    let body = apply_upstream_model(body, upstream_model);
    let model_for_cloak = if upstream_model.is_empty() {
        req.alias.as_str()
    } else {
        upstream_model
    };
    let prepared = prepare_oauth_body(body, model_for_cloak, secret.expose_secret(), opts);
    let tool_reverse: HashMap<String, String> = prepared.tool_reverse_map;

    let builder = chrome_client()
        .post(&url)
        .timeout(Duration::from_millis(overall_ms))
        .json(&prepared.body);
    let builder = apply_headers(
        builder,
        secret.expose_secret(),
        true,
        &prepared.extra_betas,
        opts,
    );

    let resp = builder.send().await.map_err(|e| {
        if e.is_timeout() {
            ProviderError::Timeout
        } else {
            ProviderError::Network(e.to_string())
        }
    })?;

    let status = resp.status().as_u16();
    if !(200..300).contains(&status) {
        let text = resp.text().await.unwrap_or_default();
        return Err(map_status(status, &text));
    }

    use eventsource_stream::Eventsource;

    let byte_stream = resp
        .bytes_stream()
        .map_err(|e| ProviderError::Network(e.to_string()));

    let mut decode_state = C::StreamState::default();
    let stream = byte_stream
        .eventsource()
        .map_err(|e| ProviderError::Network(e.to_string()))
        .map(move |result| {
            let tool_reverse = tool_reverse.clone();
            match result {
                Ok(event) => {
                    let mut data = event.data;
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
                            warn!("codec decode error: {}", e);
                            Err(ProviderError::Serialization(e.to_string()))
                        }
                    }
                }
                Err(e) => Err(e),
            }
        })
        .map(|result| match result {
            Ok(chunks) => chunks.into_iter().map(Ok).collect::<Vec<_>>(),
            Err(e) => vec![Err(e)],
        })
        .flat_map(futures::stream::iter);

    Ok((Box::pin(stream), encode_loss))
}
