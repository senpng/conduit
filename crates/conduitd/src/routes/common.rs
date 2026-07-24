//! Shared gateway request helpers: auth headers, request ids, error mapping.

use axum::{
    http::{HeaderMap, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use conduit_ir::error::GatewayError;
use serde_json::Value;
use tracing::{debug, warn, Span};

/// Response header name for the gateway correlation id.
///
/// Value equals the pipeline `request_id` / ingress ULID and is safe to share
/// with clients for log correlation (`rg <id> ~/.conduit/logs/...`).
pub const X_REQUEST_ID: &str = "x-request-id";

/// Attach `x-request-id` and record the id on the current tracing span so every
/// log under this request (including nested auth/oauth/upstream) is greppable.
pub(crate) fn stamp_request_id(request_id: &str) {
    Span::current().record("request_id", tracing::field::display(request_id));
}

pub(crate) fn with_request_id(mut response: Response, request_id: &str) -> Response {
    if let Ok(value) = HeaderValue::from_str(request_id) {
        response.headers_mut().insert(X_REQUEST_ID, value);
    }
    response
}

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

pub(crate) fn extract_key_id(headers: &HeaderMap) -> Option<String> {
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

/// True when a failure is a genuine upstream/server fault (5xx) rather than a
/// routine client error (401/404/429). Drives log severity so expected client
/// errors stay at DEBUG while real faults surface at WARN.
pub(crate) fn is_upstream_fault(err: &GatewayError) -> bool {
    status_for_gateway_error(err).is_server_error()
}



/// Map a pipeline failure to HTTP status + provider-shaped error JSON.
///
/// `error_body` is the codec's `WireCodec::error_body` (OpenAI / Anthropic /
/// Responses). `generic_type` is the fallback error type for unclassified
/// faults (`upstream_error` for OpenAI-family, `api_error` for Anthropic).
pub(crate) fn map_gateway_error(
    err: &GatewayError,
    error_body: fn(&str, Option<&str>, &str) -> Value,
    generic_type: &str,
) -> (StatusCode, Value) {
    let status = status_for_gateway_error(err);
    let body = match err {
        GatewayError::Unauthorized(msg) => {
            error_body("authentication_error", Some("invalid_api_key"), msg)
        }
        GatewayError::Routing(msg) => error_body("not_found_error", None, msg),
        GatewayError::Quota(conduit_ir::error::QuotaError::RateLimitExceeded { .. }) => {
            error_body(
                "rate_limit_error",
                Some("rate_limit_exceeded"),
                "rate limit exceeded",
            )
        }
        GatewayError::Quota(qe) => error_body("permission_error", None, &qe.to_string()),
        other => error_body(generic_type, None, &other.to_string()),
    };
    (status, body)
}

/// Log + stamp a gateway pipeline failure response.
pub(crate) fn fail_gateway(
    endpoint: &str,
    alias: &str,
    request_id: &str,
    err: &GatewayError,
    error_body: fn(&str, Option<&str>, &str) -> Value,
    generic_type: &str,
) -> Response {
    let (status, body) = map_gateway_error(err, error_body, generic_type);
    // One record per failure. Upstream faults are WARN; classified client
    // errors (401/404/429) are routine and log at DEBUG to avoid noise.
    if is_upstream_fault(err) {
        warn!(
            endpoint,
            alias = %alias,
            request_id = %request_id,
            status = %status,
            error = %err,
            "gateway request failed (upstream)"
        );
    } else {
        debug!(
            endpoint,
            alias = %alias,
            request_id = %request_id,
            status = %status,
            error = %err,
            "gateway request failed"
        );
    }
    with_request_id((status, Json(body)).into_response(), request_id)
}

/// SSE response shell shared by streaming gateway endpoints.
pub(crate) fn sse_response(body: axum::body::Body, request_id: &str) -> Response {
    with_request_id(
        Response::builder()
            .status(StatusCode::OK)
            .header("content-type", "text/event-stream")
            .header("cache-control", "no-cache")
            .header("x-accel-buffering", "no")
            .body(body)
            .unwrap(),
        request_id,
    )
}
