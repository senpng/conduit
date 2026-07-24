//! Proactive subscription quota probe for OAuth providers.
//!
//! | Provider | Endpoint | Windows |
//! |----------|----------|---------|
//! | Claude   | `GET https://api.anthropic.com/api/oauth/usage` | 5h session + 7d weekly |
//! | Codex    | `GET https://chatgpt.com/backend-api/wham/usage` | weekly 7d only (5h session removed) |
//! | Grok     | `POST https://grok.com/.../GetGrokCreditsConfig` (gRPC-web) | monthly credits |
//!
//! Mirrors CodexBar's OAuth remaining strategy: reuse the stored OAuth access
//! token, call the same private usage/billing APIs the official CLIs use, and
//! normalize to **remaining percent** (0–100) for the console / TUI.
//!
//! Not to be confused with the daemon's request **usage ledger**
//! (`conduit-store::usage_repo`): that records what this gateway *spent*, while
//! this probe reports how much subscription quota is *left upstream*. The two
//! are separate concerns and share no types — hence [`OAuthQuotaProbe`] /
//! [`QuotaWindow`] here rather than a "usage" name.

use serde_json::Value;
use secrecy::ExposeSecret;

use crate::{
    credential::{AuthMode, OAuthProviderKind, ResolvedCredential},
    error::OAuthError,
    proxy::apply_reqwest_proxy,
};

const CLAUDE_USAGE_URL: &str = "https://api.anthropic.com/api/oauth/usage";
const CLAUDE_OAUTH_BETA: &str = "oauth-2025-04-20";
const CLAUDE_ANTHROPIC_VERSION: &str = "2023-06-01";
const CODEX_USAGE_URL: &str = "https://chatgpt.com/backend-api/wham/usage";
/// CodexBar / Grok Build web billing (gRPC-web protobuf).
const GROK_BILLING_URL: &str =
    "https://grok.com/grok_api_v2.GrokBuildBilling/GetGrokCreditsConfig";

/// One rolling quota window (session / weekly / monthly credits).
#[derive(Debug, Clone, PartialEq)]
pub struct QuotaWindow {
    /// Remaining capacity as 0–100 percent.
    pub remaining_pct: f64,
    /// Used capacity as 0–100 percent (when known).
    pub used_pct: f64,
    /// Reset timestamp (RFC3339 or unix string when provided by upstream).
    pub resets_at: Option<String>,
}

/// Normalized OAuth subscription remaining-quota snapshot for display.
#[derive(Debug, Clone, PartialEq)]
pub struct OAuthQuotaProbe {
    pub provider_kind: OAuthProviderKind,
    /// Session / short window (≈5h for Claude; Codex no longer exposes this).
    pub session: Option<QuotaWindow>,
    /// Longer window: weekly for Claude/Codex, **monthly credits** for Grok.
    pub weekly: Option<QuotaWindow>,
    /// Raw source tag for the snapshot store.
    pub source: &'static str,
    /// Short excerpt for debug details.
    pub raw_excerpt: String,
}

/// Fetch subscription remaining for a resolved OAuth credential.
pub async fn fetch_oauth_usage(
    kind: OAuthProviderKind,
    resolved: &ResolvedCredential,
    proxy_url: Option<&str>,
) -> Result<OAuthQuotaProbe, OAuthError> {
    match kind {
        OAuthProviderKind::Claude => fetch_claude(resolved, proxy_url).await,
        OAuthProviderKind::Codex => fetch_codex(resolved, proxy_url).await,
        OAuthProviderKind::Xai => fetch_grok(resolved, proxy_url).await,
    }
}

async fn build_client(proxy_url: Option<&str>) -> Result<reqwest::Client, OAuthError> {
    let builder = reqwest::Client::builder().timeout(std::time::Duration::from_secs(20));
    let builder = apply_reqwest_proxy(builder, proxy_url)?;
    builder
        .build()
        .map_err(|e| OAuthError::Network(e.to_string()))
}

fn body_preview(s: &str) -> String {
    s.chars().take(200).collect()
}

/// Map HTTP status + body preview to auth refresh or provider error.
///
/// `ok` decides success: Claude/Codex use any 2xx; Grok historically requires exact 200.
fn map_probe_http_status(
    label: &str,
    status: u16,
    preview: String,
    ok: impl FnOnce(u16) -> bool,
) -> Result<(), OAuthError> {
    if status == 401 || status == 403 {
        return Err(OAuthError::TokenRefresh {
            status,
            body: preview,
        });
    }
    if !ok(status) {
        return Err(OAuthError::Provider(format!(
            "{label} HTTP {status}: {preview}"
        )));
    }
    Ok(())
}

fn http_ok_2xx(status: u16) -> bool {
    (200..300).contains(&status)
}

fn http_ok_200(status: u16) -> bool {
    status == 200
}

async fn response_text(resp: reqwest::Response) -> Result<(u16, String), OAuthError> {
    let status = resp.status().as_u16();
    let body = resp
        .text()
        .await
        .map_err(|e| OAuthError::Network(e.to_string()))?;
    Ok((status, body))
}

/// Shared JSON usage probe: build request → send → status map → parse.
async fn fetch_json_usage(
    proxy_url: Option<&str>,
    label: &str,
    build: impl FnOnce(&reqwest::Client) -> reqwest::RequestBuilder,
    parse: fn(&str) -> Result<OAuthQuotaProbe, OAuthError>,
) -> Result<OAuthQuotaProbe, OAuthError> {
    let client = build_client(proxy_url).await?;
    let resp = build(&client)
        .send()
        .await
        .map_err(|e| OAuthError::Network(e.to_string()))?;
    let (status, body) = response_text(resp).await?;
    map_probe_http_status(label, status, body_preview(&body), http_ok_2xx)?;
    parse(&body)
}

async fn fetch_claude(
    resolved: &ResolvedCredential,
    proxy_url: Option<&str>,
) -> Result<OAuthQuotaProbe, OAuthError> {
    let _ = resolved.auth_mode; // token is authoritative
    let token = resolved.access_token.expose_secret().to_string();
    fetch_json_usage(
        proxy_url,
        "claude usage",
        |client| {
            client
                .get(CLAUDE_USAGE_URL)
                .header("Authorization", format!("Bearer {token}"))
                .header("anthropic-beta", CLAUDE_OAUTH_BETA)
                .header("anthropic-version", CLAUDE_ANTHROPIC_VERSION)
                .header("User-Agent", "claude-cli/2.1.201 (external, cli)")
                .header("x-app", "cli")
        },
        parse_claude_usage,
    )
    .await
}

async fn fetch_codex(
    resolved: &ResolvedCredential,
    proxy_url: Option<&str>,
) -> Result<OAuthQuotaProbe, OAuthError> {
    let token = resolved.access_token.expose_secret().to_string();
    let account_headers: Vec<(String, String)> = resolved
        .extra_headers
        .iter()
        .filter(|(k, _)| k.eq_ignore_ascii_case("Chatgpt-Account-Id"))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    fetch_json_usage(
        proxy_url,
        "codex usage",
        |client| {
            let mut req = client
                .get(CODEX_USAGE_URL)
                .header("Authorization", format!("Bearer {token}"))
                .header("User-Agent", "codex-tui/0.135.0")
                .header("Originator", "codex-tui");
            for (k, v) in &account_headers {
                req = req.header(k.as_str(), v.as_str());
            }
            req
        },
        parse_codex_usage,
    )
    .await
}

/// Grok monthly credits via grok.com gRPC-web (CodexBar `GrokWebBillingFetcher` parity).
///
/// Uses the OAuth access token as `Authorization: Bearer …`. Empty protobuf
/// request body (5-byte gRPC-web frame with zero length).
async fn fetch_grok(
    resolved: &ResolvedCredential,
    proxy_url: Option<&str>,
) -> Result<OAuthQuotaProbe, OAuthError> {
    let _ = matches!(resolved.auth_mode, AuthMode::OAuth(OAuthProviderKind::Xai));
    let client = build_client(proxy_url).await?;
    let token = resolved.access_token.expose_secret();
    // Empty gRPC-web data frame: flag=0, length=0.
    let body = vec![0u8, 0, 0, 0, 0];
    let resp = client
        .post(GROK_BILLING_URL)
        .header("Authorization", format!("Bearer {token}"))
        .header("Origin", "https://grok.com")
        .header("Referer", "https://grok.com/?_s=usage")
        .header("Accept", "*/*")
        .header("Content-Type", "application/grpc-web+proto")
        .header("x-grpc-web", "1")
        .header("x-user-agent", "connect-es/2.1.1")
        .header("User-Agent", "Conduit/0.1")
        .body(body)
        .send()
        .await
        .map_err(|e| OAuthError::Network(e.to_string()))?;
    let status = resp.status().as_u16();
    let headers = resp.headers().clone();
    let bytes = resp
        .bytes()
        .await
        .map_err(|e| OAuthError::Network(e.to_string()))?;
    // Exact 200 only (not the broader 2xx band used by JSON probes).
    map_probe_http_status(
        "grok billing",
        status,
        body_preview(&String::from_utf8_lossy(&bytes)),
        http_ok_200,
    )?;

    // Surface gRPC status from headers or trailers before parsing.
    if let Some(err) = grpc_status_error_from_headers(&headers) {
        return Err(err);
    }
    if let Some(err) = grpc_status_error_from_trailers(&bytes) {
        return Err(err);
    }

    parse_grok_billing_protobuf(&bytes)
}

fn grpc_status_error(status: i32, message: &str) -> Option<OAuthError> {
    (status != 0).then(|| classify_grok_rpc_status(status, message))
}

fn grpc_status_error_from_headers(headers: &reqwest::header::HeaderMap) -> Option<OAuthError> {
    let status = headers
        .get("grpc-status")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.parse::<i32>().ok())?;
    let message = headers
        .get("grpc-message")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    grpc_status_error(status, message)
}

fn grpc_status_error_from_trailers(data: &[u8]) -> Option<OAuthError> {
    let fields = grpc_web_trailer_fields(data);
    let status = fields.get("grpc-status")?.parse::<i32>().ok()?;
    let message = fields.get("grpc-message").map(String::as_str).unwrap_or("");
    grpc_status_error(status, message)
}

fn classify_grok_rpc_status(status: i32, message: &str) -> OAuthError {
    let lower = message.to_ascii_lowercase();
    // gRPC UNAUTHENTICATED = 16; PERMISSION_DENIED = 7 with bad-credentials text.
    let auth_denied = status == 16
        || (status == 7
            && (lower.contains("bad-credentials")
                || lower.contains("unauthenticated")
                || (lower.contains("oauth2") && lower.contains("could not be validated"))
                || (lower.contains("access token")
                    && (lower.contains("invalid")
                        || lower.contains("expired")
                        || lower.contains("could not be validated")))));
    if auth_denied {
        return OAuthError::TokenRefresh {
            status: 401,
            body: format!("grok billing grpc-status={status}: {message}"),
        };
    }
    // FAILED_PRECONDITION = 9 "no personal team"
    if status == 9 && lower.contains("no personal team") {
        return OAuthError::Provider(
            "grok team usage is unavailable from the current billing surface".into(),
        );
    }
    OAuthError::Provider(format!(
        "grok billing RPC failed grpc-status={status}: {message}"
    ))
}

/// Parse Claude `/api/oauth/usage` JSON into windows.
pub fn parse_claude_usage(body: &str) -> Result<OAuthQuotaProbe, OAuthError> {
    let v: Value = serde_json::from_str(body)
        .map_err(|e| OAuthError::Provider(format!("claude usage json: {e}")))?;

    let mut session = window_from_utilization(v.get("five_hour"));
    let mut weekly = window_from_utilization(v.get("seven_day"));

    // When five_hour is empty, weekly becomes the primary signal (CodexBar parity).
    if session.is_none() {
        session = weekly.clone();
    }

    // Model-scoped weekly (opus/sonnet) — only if main weekly missing.
    if weekly.is_none() {
        weekly = window_from_utilization(v.get("seven_day_sonnet"))
            .or_else(|| window_from_utilization(v.get("seven_day_opus")));
    }

    if session.is_none() && weekly.is_none() {
        // Richer body may only have `limits` array.
        fill_claude_windows_from_limits(&v, &mut session, &mut weekly);
        if session.is_none() && weekly.is_none() {
            return Err(OAuthError::Provider(
                "claude usage: no five_hour/seven_day windows".into(),
            ));
        }
    }

    Ok(OAuthQuotaProbe {
        provider_kind: OAuthProviderKind::Claude,
        session,
        weekly,
        source: "oauth_usage_api",
        raw_excerpt: body.chars().take(256).collect(),
    })
}

/// Fill session/weekly from Claude `limits[]` when five_hour/seven_day are absent.
fn fill_claude_windows_from_limits(
    v: &Value,
    session: &mut Option<QuotaWindow>,
    weekly: &mut Option<QuotaWindow>,
) {
    let Some(arr) = v.get("limits").and_then(|x| x.as_array()) else {
        return;
    };
    for lim in arr {
        let kind = lim.get("kind").and_then(|x| x.as_str()).unwrap_or("");
        let Some(used) = json_f64(lim.get("percent")) else {
            continue;
        };
        let win = Some(quota_window(used, json_reset_at(lim)));
        match kind {
            "session" if session.is_none() => *session = win,
            "weekly_all" | "weekly" | "weekly_scoped" if weekly.is_none() => *weekly = win,
            _ => {}
        }
    }
}

/// Build a quota window from a used-capacity value (fraction or percent).
fn quota_window(used: f64, resets_at: Option<String>) -> QuotaWindow {
    QuotaWindow {
        used_pct: normalize_used_pct(used),
        remaining_pct: used_to_remaining(used),
        resets_at,
    }
}

/// JSON number as f64 (accepts int or float).
fn json_f64(node: Option<&Value>) -> Option<f64> {
    let node = node?;
    node.as_f64()
        .or_else(|| node.as_i64().map(|i| i as f64))
        .or_else(|| node.as_u64().map(|u| u as f64))
}

/// First matching string/number reset field among common key names.
fn json_reset_at(node: &Value) -> Option<String> {
    const KEYS: &[&str] = &["resets_at", "reset_at", "reset_after"];
    for key in KEYS {
        if let Some(v) = node.get(*key) {
            if let Some(s) = v.as_str() {
                return Some(s.to_string());
            }
            if let Some(n) = v.as_i64().or_else(|| v.as_u64().map(|u| u as i64)) {
                return Some(n.to_string());
            }
        }
    }
    None
}

fn window_from_utilization(node: Option<&Value>) -> Option<QuotaWindow> {
    let node = node?;
    if node.is_null() {
        return None;
    }
    let used = json_f64(node.get("utilization")).unwrap_or(0.0);
    Some(quota_window(used, json_reset_at(node)))
}

/// Parse Codex `wham/usage` JSON into session/weekly windows.
///
/// Product change: Codex dropped the ≈5h session cap; only the weekly (≈7d)
/// limit remains. We still classify a short `limit_window_seconds` as session
/// if upstream ever returns one (legacy dual-window payloads).
pub fn parse_codex_usage(body: &str) -> Result<OAuthQuotaProbe, OAuthError> {
    let v: Value = serde_json::from_str(body)
        .map_err(|e| OAuthError::Provider(format!("codex usage json: {e}")))?;

    // Common shapes:
    // 1) { "rate_limit": { "primary_window": {...}, "secondary_window": {...} } }
    // 2) { "rate_limits": { "primary": {...}, "secondary": {...} } }
    // 3) top-level primary_window / secondary_window
    let rl = v
        .get("rate_limit")
        .or_else(|| v.get("rate_limits"))
        .unwrap_or(&v);

    let primary = rl
        .get("primary_window")
        .or_else(|| rl.get("primary"))
        .or_else(|| v.get("primary_window"));
    let secondary = rl
        .get("secondary_window")
        .or_else(|| rl.get("secondary"))
        .or_else(|| v.get("secondary_window"));

    let (session, weekly) = classify_codex_windows(primary, secondary);
    if session.is_none() && weekly.is_none() {
        return Err(OAuthError::Provider(
            "codex usage: no rate_limit windows".into(),
        ));
    }
    Ok(OAuthQuotaProbe {
        provider_kind: OAuthProviderKind::Codex,
        session,
        weekly,
        source: "oauth_usage_api",
        raw_excerpt: body.chars().take(256).collect(),
    })
}

/// Map primary/secondary Codex windows → (session, weekly).
///
/// Duration bands: ~5h (3600..43200) → session; ≥1d → weekly. Without durations,
/// dual windows keep legacy primary=session/secondary=weekly; a single window is weekly.
fn classify_codex_windows(
    primary: Option<&Value>,
    secondary: Option<&Value>,
) -> (Option<QuotaWindow>, Option<QuotaWindow>) {
    let nodes: Vec<&Value> = [primary, secondary].into_iter().flatten().collect();
    if nodes.iter().any(|n| codex_window_secs(n).is_some()) {
        let mut session = None;
        let mut weekly = None;
        for node in &nodes {
            let win = window_from_codex_node(Some(node));
            match codex_window_secs(node) {
                Some(secs) if (3_600..43_200).contains(&secs) => session = win,
                Some(secs) if secs >= 86_400 => weekly = win,
                // Unknown band: prefer weekly, then session.
                _ if weekly.is_none() => weekly = win,
                _ if session.is_none() => session = win,
                _ => {}
            }
        }
        return (session, weekly);
    }
    match (
        window_from_codex_node(primary),
        window_from_codex_node(secondary),
    ) {
        (Some(p), Some(s)) => (Some(p), Some(s)),
        (Some(only), None) | (None, Some(only)) => (None, Some(only)),
        (None, None) => (None, None),
    }
}

fn codex_window_secs(node: &Value) -> Option<u64> {
    node.get("limit_window_seconds")
        .or_else(|| node.get("limit_window_secs"))
        .and_then(|x| x.as_u64().or_else(|| x.as_i64().map(|i| i.max(0) as u64)))
}

fn window_from_codex_node(node: Option<&Value>) -> Option<QuotaWindow> {
    let node = node?;
    if node.is_null() {
        return None;
    }
    // Prefer used_percent / used_pct; fall back to utilization.
    let used = json_f64(node.get("used_percent"))
        .or_else(|| json_f64(node.get("used_pct")))
        .or_else(|| json_f64(node.get("utilization")))?;
    Some(quota_window(used, json_reset_at(node)))
}

/// Parse Grok gRPC-web protobuf billing response into a monthly credits window.
///
/// Strategy (CodexBar parity): scan nested protobuf for:
/// - fixed32 floats in 0..=100 → credit_usage_percent (used %)
/// - varint unix timestamps ≈ 1.7e9..2.1e9 → period end / reset
/// - empty usage period with only reset → treat used as 0%
pub fn parse_grok_billing_protobuf(data: &[u8]) -> Result<OAuthQuotaProbe, OAuthError> {
    let mut payloads = grpc_web_data_frames(data);
    if payloads.is_empty() && looks_like_protobuf(data) {
        payloads.push(data.to_vec());
    }
    if payloads.is_empty() {
        return Err(OAuthError::Provider(
            "grok billing: empty protobuf payload".into(),
        ));
    }

    let mut scan = ProtobufScan::default();
    for payload in &payloads {
        scan.merge(scan_protobuf(payload, 0, &[]));
    }

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    let resets_at = grok_reset_from_scan(&scan, now);
    let used = match grok_used_pct_from_scan(&scan) {
        Some(u) => u,
        None if grok_empty_usage_ok(&scan, resets_at.is_some()) => 0.0,
        None => {
            return Err(OAuthError::Provider(
                "grok billing: could not parse credit usage percent".into(),
            ));
        }
    };

    let hex_preview: String = data
        .iter()
        .take(48)
        .map(|b| format!("{b:02x}"))
        .collect::<Vec<_>>()
        .join(" ");

    Ok(OAuthQuotaProbe {
        provider_kind: OAuthProviderKind::Xai,
        // Grok has no 5h session window from this endpoint.
        session: None,
        // Store monthly credits in the longer-window slot.
        weekly: Some(quota_window(used, resets_at)),
        source: "oauth_billing_api",
        raw_excerpt: format!("grpc-web {} bytes; head={hex_preview}", data.len()),
    })
}

/// Empty usage period with only a reset timestamp → treat used as 0%.
fn grok_empty_usage_ok(scan: &ProtobufScan, has_reset: bool) -> bool {
    if !has_reset || !scan.fixed32.is_empty() {
        return false;
    }
    scan.varints.iter().any(|f| {
        f.path.starts_with(&[1, 6])
            || (f.path.as_slice() == [1, 8, 1] && (f.value == 1 || f.value == 2))
    })
}

/// Shallowest fixed32 path ending in field 1 with value in 0..=100.
fn grok_used_pct_from_scan(scan: &ProtobufScan) -> Option<f64> {
    let mut candidates: Vec<(usize, usize, f32)> = scan
        .fixed32
        .iter()
        .filter(|f| {
            f.path.last() == Some(&1) && f.value.is_finite() && (0.0..=100.0).contains(&f.value)
        })
        .map(|f| (f.path.len(), f.order, f.value))
        .collect();
    candidates.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)));
    candidates.first().map(|c| c.2 as f64)
}

/// Prefer future reset at path [1,5,1] (CodexBar), else earliest future, else any.
fn grok_reset_from_scan(scan: &ProtobufScan, now: u64) -> Option<String> {
    let resets: Vec<(Vec<u64>, u64)> = scan
        .varints
        .iter()
        .filter(|f| (1_700_000_000..=2_100_000_000).contains(&f.value))
        .map(|f| (f.path.clone(), f.value))
        .collect();
    let prefer = |items: &[(Vec<u64>, u64)]| {
        items
            .iter()
            .find(|(path, _)| path.as_slice() == [1, 5, 1])
            .or_else(|| items.iter().min_by_key(|(_, ts)| *ts))
            .map(|(_, ts)| ts.to_string())
    };
    let future: Vec<_> = resets
        .iter()
        .filter(|(_, ts)| *ts > now)
        .cloned()
        .collect();
    prefer(&future).or_else(|| prefer(&resets))
}

// ── gRPC-web / protobuf helpers (CodexBar GrokWebBillingFetcher) ────────────

/// Parse one gRPC-web frame header: `(flags, payload_range, next_index)`.
fn grpc_web_frame_at(data: &[u8], index: usize) -> Option<(u8, std::ops::Range<usize>, usize)> {
    if index + 5 > data.len() {
        return None;
    }
    let flags = data[index];
    let length = u32::from_be_bytes([
        data[index + 1],
        data[index + 2],
        data[index + 3],
        data[index + 4],
    ]) as usize;
    let start = index + 5;
    let end = start.checked_add(length)?;
    if end > data.len() {
        return None;
    }
    Some((flags, start..end, end))
}

/// Walk gRPC-web frames. On a truncated/invalid header:
/// - `strict`: abort and return `false` (data-frame path — bad stream)
/// - non-strict: stop early and return `true` (trailers are best-effort)
fn for_each_grpc_web_frame(
    data: &[u8],
    strict: bool,
    mut visit: impl FnMut(u8, &[u8]),
) -> bool {
    let mut index = 0usize;
    while index < data.len() {
        let Some((flags, range, next)) = grpc_web_frame_at(data, index) else {
            return !strict;
        };
        visit(flags, &data[range]);
        index = next;
    }
    true
}

fn grpc_web_data_frames(data: &[u8]) -> Vec<Vec<u8>> {
    let mut frames = Vec::new();
    let ok = for_each_grpc_web_frame(data, true, |flags, payload| {
        // Trailer frames have high bit set (0x80).
        if flags & 0x80 == 0 {
            frames.push(payload.to_vec());
        }
    });
    if ok {
        frames
    } else {
        Vec::new()
    }
}

fn grpc_web_trailer_fields(data: &[u8]) -> std::collections::HashMap<String, String> {
    let mut fields = std::collections::HashMap::new();
    let _ = for_each_grpc_web_frame(data, false, |flags, payload| {
        // Trailer frames have high bit set (0x80).
        if flags & 0x80 == 0 {
            return;
        }
        let Ok(text) = std::str::from_utf8(payload) else {
            return;
        };
        for line in text.lines().filter(|l| !l.is_empty()) {
            if let Some((k, v)) = line.split_once(':') {
                fields.insert(k.trim().to_ascii_lowercase(), v.trim().to_string());
            }
        }
    });
    fields
}

fn looks_like_protobuf(data: &[u8]) -> bool {
    let Some(&first) = data.first() else {
        return false;
    };
    let field_number = first >> 3;
    let wire_type = first & 0x07;
    field_number > 0 && matches!(wire_type, 0 | 1 | 2 | 5)
}

#[derive(Default)]
struct ProtobufScan {
    fixed32: Vec<Fixed32Field>,
    varints: Vec<VarintField>,
}

struct Fixed32Field {
    path: Vec<u64>,
    value: f32,
    order: usize,
}

struct VarintField {
    path: Vec<u64>,
    value: u64,
}

impl ProtobufScan {
    fn merge(&mut self, other: ProtobufScan) {
        self.fixed32.extend(other.fixed32);
        self.varints.extend(other.varints);
    }
}

fn scan_protobuf(data: &[u8], depth: u8, path: &[u64]) -> ProtobufScan {
    let mut scan = ProtobufScan::default();
    let mut index = 0usize;
    let mut order = 0usize;
    // On parse failure, skip one byte and resync (CodexBar-style best-effort scan).
    let resync = |index: &mut usize, field_start: usize| {
        *index = field_start + 1;
    };
    while index < data.len() {
        let field_start = index;
        let Some(key) = read_varint(data, &mut index) else {
            resync(&mut index, field_start);
            continue;
        };
        if key == 0 {
            resync(&mut index, field_start);
            continue;
        }
        let field_number = key >> 3;
        let wire_type = key & 0x07;
        let mut field_path = path.to_vec();
        field_path.push(field_number);

        match wire_type {
            0 => match read_varint(data, &mut index) {
                Some(value) => scan.varints.push(VarintField {
                    path: field_path,
                    value,
                }),
                None => resync(&mut index, field_start),
            },
            1 => {
                // fixed64 — skip (not used for billing %)
                if index + 8 > data.len() {
                    break;
                }
                index += 8;
            }
            2 => {
                let Some(length) = read_varint(data, &mut index) else {
                    resync(&mut index, field_start);
                    continue;
                };
                let length = length as usize;
                if length > data.len().saturating_sub(index) {
                    resync(&mut index, field_start);
                    continue;
                }
                let end = index + length;
                if depth < 4 {
                    scan.merge(scan_protobuf(&data[index..end], depth + 1, &field_path));
                }
                index = end;
            }
            5 => {
                if index + 4 > data.len() {
                    break;
                }
                let bits = u32::from_le_bytes(data[index..index + 4].try_into().unwrap());
                scan.fixed32.push(Fixed32Field {
                    path: field_path,
                    value: f32::from_bits(bits),
                    order,
                });
                order += 1;
                index += 4;
            }
            _ => resync(&mut index, field_start),
        }
    }
    scan
}

fn read_varint(data: &[u8], index: &mut usize) -> Option<u64> {
    let mut value: u64 = 0;
    let mut shift: u32 = 0;
    while *index < data.len() && shift < 64 {
        let byte = data[*index];
        *index += 1;
        value |= u64::from(byte & 0x7f) << shift;
        if byte & 0x80 == 0 {
            return Some(value);
        }
        shift += 7;
    }
    None
}

/// Normalize a used value to 0–100 percent.
///
/// Values in `(0, 1]` are treated as fractions (legacy samples used 0.42);
/// values `> 1` are already percent (Claude Code HUD uses 5.0, 34.0, …).
pub fn normalize_used_pct(used: f64) -> f64 {
    if !used.is_finite() {
        return 0.0;
    }
    if used > 1.0 {
        used.clamp(0.0, 100.0)
    } else if used < 0.0 {
        0.0
    } else {
        // 0..=1 → fraction scale
        (used * 100.0).clamp(0.0, 100.0)
    }
}

pub fn used_to_remaining(used: f64) -> f64 {
    (100.0 - normalize_used_pct(used)).clamp(0.0, 100.0)
}

/// Compact one-line summary for list columns.
///
/// Claude: `5h 95% · 7d 66%`
/// Codex: `7d 60%` (weekly only; session only if legacy short window present)
/// Grok: `mo 72%` (monthly credits remaining)
pub fn format_remaining_short(usage: &OAuthQuotaProbe) -> String {
    match usage.provider_kind {
        OAuthProviderKind::Xai => {
            if let Some(w) = &usage.weekly {
                return format!("mo {:.0}%", w.remaining_pct);
            }
            if let Some(s) = &usage.session {
                return format!("credits {:.0}%", s.remaining_pct);
            }
            "—".into()
        }
        _ => {
            let mut parts = Vec::new();
            if let Some(s) = &usage.session {
                parts.push(format!("5h {:.0}%", s.remaining_pct));
            }
            if let Some(w) = &usage.weekly {
                parts.push(format!("7d {:.0}%", w.remaining_pct));
            }
            if parts.is_empty() {
                "—".into()
            } else {
                parts.join(" · ")
            }
        }
    }
}

/// True when the snapshot source is Grok monthly billing (label as `mo` not `7d`).
pub fn is_billing_source(source: &str) -> bool {
    let s = source.to_ascii_lowercase();
    s.contains("billing") || s.contains("grok")
}

/// Build extra headers for a usage probe from a raw credential (Codex account id).
pub fn usage_headers_for_cred(
    kind: OAuthProviderKind,
    cred: &crate::credential::OAuthCredential,
) -> Vec<(String, String)> {
    crate::credential::oauth_extra_headers(kind, cred)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claude_five_seven_percent_scale() {
        let body = r#"{
          "five_hour": { "utilization": 5.0, "resets_at": "2026-07-03T17:09:59Z" },
          "seven_day": { "utilization": 34.0, "resets_at": "2026-07-09T10:59:59Z" }
        }"#;
        let u = parse_claude_usage(body).unwrap();
        assert!((u.session.as_ref().unwrap().remaining_pct - 95.0).abs() < 0.01);
        assert!((u.weekly.as_ref().unwrap().remaining_pct - 66.0).abs() < 0.01);
        assert_eq!(
            u.session.as_ref().unwrap().resets_at.as_deref(),
            Some("2026-07-03T17:09:59Z")
        );
    }

    #[test]
    fn claude_fraction_scale() {
        let body = r#"{
          "five_hour": { "utilization": 0.42 },
          "seven_day": { "utilization": 0.61 }
        }"#;
        let u = parse_claude_usage(body).unwrap();
        assert!((u.session.as_ref().unwrap().remaining_pct - 58.0).abs() < 0.01);
        assert!((u.weekly.as_ref().unwrap().remaining_pct - 39.0).abs() < 0.01);
    }

    #[test]
    fn codex_legacy_primary_secondary() {
        // Legacy dual-window payload (pre-removal of 5h session).
        let body = r#"{
          "rate_limit": {
            "primary_window": {
              "used_percent": 12.5,
              "limit_window_seconds": 18000,
              "reset_at": "2026-07-20T12:00:00Z"
            },
            "secondary_window": {
              "used_percent": 40.0,
              "limit_window_seconds": 604800,
              "reset_at": "2026-07-26T00:00:00Z"
            }
          }
        }"#;
        let u = parse_codex_usage(body).unwrap();
        assert!((u.session.as_ref().unwrap().remaining_pct - 87.5).abs() < 0.01);
        assert!((u.weekly.as_ref().unwrap().remaining_pct - 60.0).abs() < 0.01);
        assert_eq!(format_remaining_short(&u), "5h 88% · 7d 60%");
    }

    #[test]
    fn codex_weekly_only_primary() {
        // Current product: 5h session removed; primary is the weekly window.
        let body = r#"{
          "rate_limit": {
            "primary_window": {
              "used_percent": 40.0,
              "limit_window_seconds": 604800,
              "reset_at": "2026-07-26T00:00:00Z"
            }
          }
        }"#;
        let u = parse_codex_usage(body).unwrap();
        assert!(u.session.is_none(), "weekly primary must not be labeled 5h");
        assert!((u.weekly.as_ref().unwrap().remaining_pct - 60.0).abs() < 0.01);
        assert_eq!(format_remaining_short(&u), "7d 60%");
    }

    #[test]
    fn codex_single_window_no_duration_is_weekly() {
        let body = r#"{
          "rate_limit": {
            "primary_window": {
              "used_percent": 25.0,
              "reset_at": "2026-07-26T00:00:00Z"
            }
          }
        }"#;
        let u = parse_codex_usage(body).unwrap();
        assert!(u.session.is_none());
        assert!((u.weekly.as_ref().unwrap().remaining_pct - 75.0).abs() < 0.01);
        assert_eq!(format_remaining_short(&u), "7d 75%");
    }

    #[test]
    fn normalize_edge() {
        assert!((normalize_used_pct(0.0) - 0.0).abs() < 1e-9);
        assert!((normalize_used_pct(1.0) - 100.0).abs() < 1e-9);
        assert!((normalize_used_pct(50.0) - 50.0).abs() < 1e-9);
    }

    /// Build a minimal protobuf: field 1 (message) containing field 1 fixed32 = 28.0.
    /// Wrapped in a gRPC-web data frame.
    #[test]
    fn grok_billing_fixed32_percent() {
        // Inner message: tag field1 wire5 (0x0d) + float 28.0 le
        let mut inner = Vec::new();
        // field 1, wire type 5 → key = (1<<3)|5 = 13 = 0x0d
        inner.push(0x0d);
        inner.extend_from_slice(&28.0f32.to_le_bytes());

        // Outer: field 1, wire type 2 (len-delimited) containing inner
        let mut proto = Vec::new();
        // key = (1<<3)|2 = 10 = 0x0a
        proto.push(0x0a);
        proto.push(inner.len() as u8);
        proto.extend_from_slice(&inner);

        // gRPC-web frame: flag 0 + be length + payload
        let mut frame = vec![0u8];
        let len = (proto.len() as u32).to_be_bytes();
        frame.extend_from_slice(&len);
        frame.extend_from_slice(&proto);

        let u = parse_grok_billing_protobuf(&frame).unwrap();
        assert_eq!(u.provider_kind, OAuthProviderKind::Xai);
        assert!(u.session.is_none());
        let weekly = u.weekly.as_ref().unwrap();
        assert!((weekly.used_pct - 28.0).abs() < 0.01);
        assert!((weekly.remaining_pct - 72.0).abs() < 0.01);
        assert_eq!(u.source, "oauth_billing_api");
        assert_eq!(format_remaining_short(&u), "mo 72%");
    }

    #[test]
    fn is_billing_source_detects() {
        assert!(is_billing_source("oauth_billing_api"));
        assert!(!is_billing_source("oauth_usage_api"));
    }
}
