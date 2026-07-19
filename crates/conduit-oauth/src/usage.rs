//! Proactive subscription quota probe for OAuth providers.
//!
//! | Provider | Endpoint | Windows |
//! |----------|----------|---------|
//! | Claude   | `GET https://api.anthropic.com/api/oauth/usage` | 5h session + 7d weekly |
//! | Codex    | `GET https://chatgpt.com/backend-api/wham/usage` | 5h primary + 7d secondary |
//! | Grok     | `POST https://grok.com/.../GetGrokCreditsConfig` (gRPC-web) | monthly credits |
//!
//! Mirrors CodexBar's OAuth remaining strategy: reuse the stored OAuth access
//! token, call the same private usage/billing APIs the official CLIs use, and
//! normalize to **remaining percent** (0–100) for the console / TUI.

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

/// One rolling usage window (session / weekly / monthly credits).
#[derive(Debug, Clone, PartialEq)]
pub struct UsageWindow {
    /// Remaining capacity as 0–100 percent.
    pub remaining_pct: f64,
    /// Used capacity as 0–100 percent (when known).
    pub used_pct: f64,
    /// Reset timestamp (RFC3339 or unix string when provided by upstream).
    pub resets_at: Option<String>,
}

/// Normalized OAuth subscription usage for display.
#[derive(Debug, Clone, PartialEq)]
pub struct OauthUsage {
    pub provider_kind: OAuthProviderKind,
    /// Session / short window (≈5h for Claude/Codex).
    pub session: Option<UsageWindow>,
    /// Longer window: weekly for Claude/Codex, **monthly credits** for Grok.
    pub weekly: Option<UsageWindow>,
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
) -> Result<OauthUsage, OAuthError> {
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

async fn fetch_claude(
    resolved: &ResolvedCredential,
    proxy_url: Option<&str>,
) -> Result<OauthUsage, OAuthError> {
    let _ = resolved.auth_mode; // token is authoritative
    let client = build_client(proxy_url).await?;
    let token = resolved.access_token.expose_secret();
    let resp = client
        .get(CLAUDE_USAGE_URL)
        .header("Authorization", format!("Bearer {token}"))
        .header("anthropic-beta", CLAUDE_OAUTH_BETA)
        .header("anthropic-version", CLAUDE_ANTHROPIC_VERSION)
        .header("User-Agent", "claude-cli/2.1.201 (external, cli)")
        .header("x-app", "cli")
        .send()
        .await
        .map_err(|e| OAuthError::Network(e.to_string()))?;
    let status = resp.status().as_u16();
    let body = resp
        .text()
        .await
        .map_err(|e| OAuthError::Network(e.to_string()))?;
    if status == 401 || status == 403 {
        return Err(OAuthError::TokenRefresh {
            status,
            body: body.chars().take(200).collect(),
        });
    }
    if !(200..300).contains(&status) {
        return Err(OAuthError::Provider(format!(
            "claude usage HTTP {status}: {}",
            body.chars().take(200).collect::<String>()
        )));
    }
    parse_claude_usage(&body)
}

async fn fetch_codex(
    resolved: &ResolvedCredential,
    proxy_url: Option<&str>,
) -> Result<OauthUsage, OAuthError> {
    let client = build_client(proxy_url).await?;
    let token = resolved.access_token.expose_secret();
    let mut req = client
        .get(CODEX_USAGE_URL)
        .header("Authorization", format!("Bearer {token}"))
        .header("User-Agent", "codex-tui/0.135.0")
        .header("Originator", "codex-tui");
    // Inject Chatgpt-Account-Id when present on the credential extras.
    for (k, v) in &resolved.extra_headers {
        if k.eq_ignore_ascii_case("Chatgpt-Account-Id") {
            req = req.header(k.as_str(), v.as_str());
        }
    }
    let resp = req
        .send()
        .await
        .map_err(|e| OAuthError::Network(e.to_string()))?;
    let status = resp.status().as_u16();
    let body = resp
        .text()
        .await
        .map_err(|e| OAuthError::Network(e.to_string()))?;
    if status == 401 || status == 403 {
        return Err(OAuthError::TokenRefresh {
            status,
            body: body.chars().take(200).collect(),
        });
    }
    if !(200..300).contains(&status) {
        return Err(OAuthError::Provider(format!(
            "codex usage HTTP {status}: {}",
            body.chars().take(200).collect::<String>()
        )));
    }
    parse_codex_usage(&body)
}

/// Grok monthly credits via grok.com gRPC-web (CodexBar `GrokWebBillingFetcher` parity).
///
/// Uses the OAuth access token as `Authorization: Bearer …`. Empty protobuf
/// request body (5-byte gRPC-web frame with zero length).
async fn fetch_grok(
    resolved: &ResolvedCredential,
    proxy_url: Option<&str>,
) -> Result<OauthUsage, OAuthError> {
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

    if status == 401 || status == 403 {
        let preview = String::from_utf8_lossy(&bytes);
        return Err(OAuthError::TokenRefresh {
            status,
            body: preview.chars().take(200).collect(),
        });
    }
    if status != 200 {
        let preview = String::from_utf8_lossy(&bytes);
        return Err(OAuthError::Provider(format!(
            "grok billing HTTP {status}: {}",
            preview.chars().take(200).collect::<String>()
        )));
    }

    // Surface gRPC status from headers or trailers before parsing.
    if let Some(err) = grpc_status_error_from_headers(&headers) {
        return Err(err);
    }
    if let Some(err) = grpc_status_error_from_trailers(&bytes) {
        return Err(err);
    }

    parse_grok_billing_protobuf(&bytes)
}

fn grpc_status_error_from_headers(headers: &reqwest::header::HeaderMap) -> Option<OAuthError> {
    let status = headers
        .get("grpc-status")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.parse::<i32>().ok())?;
    if status == 0 {
        return None;
    }
    let message = headers
        .get("grpc-message")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    Some(classify_grok_rpc_status(status, &message))
}

fn grpc_status_error_from_trailers(data: &[u8]) -> Option<OAuthError> {
    let fields = grpc_web_trailer_fields(data);
    let status = fields.get("grpc-status")?.parse::<i32>().ok()?;
    if status == 0 {
        return None;
    }
    let message = fields.get("grpc-message").cloned().unwrap_or_default();
    Some(classify_grok_rpc_status(status, &message))
}

fn classify_grok_rpc_status(status: i32, message: &str) -> OAuthError {
    let lower = message.to_ascii_lowercase();
    // gRPC UNAUTHENTICATED = 16; PERMISSION_DENIED = 7 with bad-credentials text.
    if status == 16
        || (status == 7
            && (lower.contains("bad-credentials")
                || lower.contains("unauthenticated")
                || (lower.contains("oauth2") && lower.contains("could not be validated"))
                || (lower.contains("access token")
                    && (lower.contains("invalid")
                        || lower.contains("expired")
                        || lower.contains("could not be validated")))))
    {
        return OAuthError::TokenRefresh {
            status: 401,
            body: format!("grok billing grpc-status={status}: {message}"),
        };
    }
    // FAILED_PRECONDITION = 9 "no personal team"
    if status == 9
        && (lower.contains("no personal team") || lower.trim() == "no personal team.")
    {
        return OAuthError::Provider(
            "grok team usage is unavailable from the current billing surface".into(),
        );
    }
    OAuthError::Provider(format!(
        "grok billing RPC failed grpc-status={status}: {message}"
    ))
}

/// Parse Claude `/api/oauth/usage` JSON into windows.
pub fn parse_claude_usage(body: &str) -> Result<OauthUsage, OAuthError> {
    let v: Value = serde_json::from_str(body)
        .map_err(|e| OAuthError::Provider(format!("claude usage json: {e}")))?;

    let mut session = window_from_utilization(v.get("five_hour"));
    let mut weekly = window_from_utilization(v.get("seven_day"));

    // When five_hour is empty, weekly becomes the primary signal (CodexBar parity).
    if session.is_none() {
        if let Some(w) = weekly.clone() {
            session = Some(w);
        }
    }

    // Model-scoped weekly (opus/sonnet) — only if main weekly missing.
    if weekly.is_none() {
        weekly = window_from_utilization(v.get("seven_day_sonnet"))
            .or_else(|| window_from_utilization(v.get("seven_day_opus")));
    }

    if session.is_none() && weekly.is_none() {
        // Richer body may only have `limits` array.
        let mut lim_session = None;
        let mut lim_weekly = None;
        if let Some(arr) = v.get("limits").and_then(|x| x.as_array()) {
            for lim in arr {
                let kind = lim.get("kind").and_then(|x| x.as_str()).unwrap_or("");
                let pct = lim
                    .get("percent")
                    .and_then(|x| x.as_f64())
                    .or_else(|| lim.get("percent").and_then(|x| x.as_i64()).map(|i| i as f64));
                let Some(used) = pct else { continue };
                let resets = lim
                    .get("resets_at")
                    .and_then(|x| x.as_str())
                    .map(|s| s.to_string());
                let win = Some(UsageWindow {
                    used_pct: normalize_used_pct(used),
                    remaining_pct: used_to_remaining(used),
                    resets_at: resets,
                });
                match kind {
                    "session" if lim_session.is_none() => lim_session = win,
                    "weekly_all" | "weekly" if lim_weekly.is_none() => lim_weekly = win,
                    "weekly_scoped" if lim_weekly.is_none() => lim_weekly = win,
                    _ => {}
                }
            }
        }
        session = lim_session;
        weekly = lim_weekly;
        if session.is_none() && weekly.is_none() {
            return Err(OAuthError::Provider(
                "claude usage: no five_hour/seven_day windows".into(),
            ));
        }
    }

    Ok(OauthUsage {
        provider_kind: OAuthProviderKind::Claude,
        session,
        weekly,
        source: "oauth_usage_api",
        raw_excerpt: body.chars().take(256).collect(),
    })
}

fn window_from_utilization(node: Option<&Value>) -> Option<UsageWindow> {
    let node = node?;
    if node.is_null() {
        return None;
    }
    let used = node
        .get("utilization")
        .and_then(|x| x.as_f64())
        .or_else(|| {
            node.get("utilization")
                .and_then(|x| x.as_i64())
                .map(|i| i as f64)
        })
        .unwrap_or(0.0);
    let resets_at = node
        .get("resets_at")
        .and_then(|x| x.as_str())
        .map(|s| s.to_string());
    Some(UsageWindow {
        used_pct: normalize_used_pct(used),
        remaining_pct: used_to_remaining(used),
        resets_at,
    })
}

/// Parse Codex `wham/usage` JSON into session/weekly windows.
pub fn parse_codex_usage(body: &str) -> Result<OauthUsage, OAuthError> {
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

    let mut session = window_from_codex_node(primary);
    let mut weekly = window_from_codex_node(secondary);

    // Prefer duration-based classification when limit_window_seconds present.
    for node in [primary, secondary].into_iter().flatten() {
        let secs = node
            .get("limit_window_seconds")
            .or_else(|| node.get("limit_window_secs"))
            .and_then(|x| x.as_u64().or_else(|| x.as_i64().map(|i| i.max(0) as u64)));
        let Some(secs) = secs else { continue };
        let win = window_from_codex_node(Some(node));
        // ~5h = 18000, ~7d = 604800 — allow slack.
        if (3_600..43_200).contains(&secs) {
            session = win;
        } else if secs >= 86_400 {
            weekly = win;
        }
    }

    if session.is_none() && weekly.is_none() {
        return Err(OAuthError::Provider(
            "codex usage: no rate_limit windows".into(),
        ));
    }
    Ok(OauthUsage {
        provider_kind: OAuthProviderKind::Codex,
        session,
        weekly,
        source: "oauth_usage_api",
        raw_excerpt: body.chars().take(256).collect(),
    })
}

fn window_from_codex_node(node: Option<&Value>) -> Option<UsageWindow> {
    let node = node?;
    if node.is_null() {
        return None;
    }
    let used = node
        .get("used_percent")
        .or_else(|| node.get("used_pct"))
        .or_else(|| node.get("utilization"))
        .and_then(|x| x.as_f64())
        .or_else(|| {
            node.get("used_percent")
                .and_then(|x| x.as_i64())
                .map(|i| i as f64)
        })?;
    let resets_at = node
        .get("reset_at")
        .or_else(|| node.get("resets_at"))
        .or_else(|| node.get("reset_after"))
        .and_then(|x| {
            if let Some(s) = x.as_str() {
                Some(s.to_string())
            } else if let Some(n) = x.as_i64().or_else(|| x.as_u64().map(|u| u as i64)) {
                Some(n.to_string())
            } else {
                None
            }
        });
    Some(UsageWindow {
        used_pct: normalize_used_pct(used),
        remaining_pct: used_to_remaining(used),
        resets_at,
    })
}

/// Parse Grok gRPC-web protobuf billing response into a monthly credits window.
///
/// Strategy (CodexBar parity): scan nested protobuf for:
/// - fixed32 floats in 0..=100 → credit_usage_percent (used %)
/// - varint unix timestamps ≈ 1.7e9..2.1e9 → period end / reset
/// - empty usage period with only reset → treat used as 0%
pub fn parse_grok_billing_protobuf(data: &[u8]) -> Result<OauthUsage, OAuthError> {
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

    // Prefer shallowest fixed32 path ending in field 1 with value in 0..=100.
    let mut candidates: Vec<(usize, usize, f32)> = scan
        .fixed32
        .iter()
        .filter(|f| {
            f.path.last() == Some(&1)
                && f.value.is_finite()
                && (0.0..=100.0).contains(&f.value)
        })
        .map(|f| (f.path.len(), f.order, f.value))
        .collect();
    candidates.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)));
    let parsed_used = candidates.first().map(|c| c.2 as f64);

    let reset_candidates: Vec<(Vec<u64>, u64)> = scan
        .varints
        .iter()
        .filter(|f| (1_700_000_000..=2_100_000_000).contains(&f.value))
        .map(|f| (f.path.clone(), f.value))
        .collect();
    // Prefer future resets; prefer path [1,5,1] (CodexBar).
    let future: Vec<_> = reset_candidates
        .iter()
        .filter(|(_, ts)| *ts > now)
        .cloned()
        .collect();
    let preferred = future
        .iter()
        .find(|(path, _)| path.as_slice() == [1, 5, 1])
        .or_else(|| future.iter().min_by_key(|(_, ts)| *ts))
        .or_else(|| {
            reset_candidates
                .iter()
                .find(|(path, _)| path.as_slice() == [1, 5, 1])
        })
        .or_else(|| reset_candidates.iter().min_by_key(|(_, ts)| *ts));
    let resets_at = preferred.map(|(_, ts)| ts.to_string());

    let has_usage_period = scan.varints.iter().any(|f| {
        f.path.starts_with(&[1, 6])
            || (f.path.as_slice() == [1, 8, 1] && (f.value == 1 || f.value == 2))
    });
    let no_usage_yet =
        parsed_used.is_none() && scan.fixed32.is_empty() && resets_at.is_some() && has_usage_period;

    let used = match parsed_used {
        Some(u) => u,
        None if no_usage_yet => 0.0,
        None => {
            return Err(OAuthError::Provider(
                "grok billing: could not parse credit usage percent".into(),
            ));
        }
    };

    let used_pct = normalize_used_pct(used);
    let remaining_pct = used_to_remaining(used);
    let monthly = UsageWindow {
        remaining_pct,
        used_pct,
        resets_at,
    };

    let hex_preview: String = data
        .iter()
        .take(48)
        .map(|b| format!("{b:02x}"))
        .collect::<Vec<_>>()
        .join(" ");

    Ok(OauthUsage {
        provider_kind: OAuthProviderKind::Xai,
        // Grok has no 5h session window from this endpoint.
        session: None,
        // Store monthly credits in the longer-window slot.
        weekly: Some(monthly),
        source: "oauth_billing_api",
        raw_excerpt: format!("grpc-web {} bytes; head={hex_preview}", data.len()),
    })
}

// ── gRPC-web / protobuf helpers (CodexBar GrokWebBillingFetcher) ────────────

fn grpc_web_data_frames(data: &[u8]) -> Vec<Vec<u8>> {
    let mut frames = Vec::new();
    let mut index = 0usize;
    while index < data.len() {
        if index + 5 > data.len() {
            return Vec::new();
        }
        let flags = data[index];
        let length = u32::from_be_bytes([
            data[index + 1],
            data[index + 2],
            data[index + 3],
            data[index + 4],
        ]) as usize;
        let start = index + 5;
        let end = start.saturating_add(length);
        if end > data.len() {
            return Vec::new();
        }
        // Trailer frames have high bit set (0x80).
        if flags & 0x80 == 0 {
            frames.push(data[start..end].to_vec());
        }
        index = end;
    }
    frames
}

fn grpc_web_trailer_fields(data: &[u8]) -> std::collections::HashMap<String, String> {
    let mut fields = std::collections::HashMap::new();
    let mut index = 0usize;
    while index + 5 <= data.len() {
        let flags = data[index];
        let length = u32::from_be_bytes([
            data[index + 1],
            data[index + 2],
            data[index + 3],
            data[index + 4],
        ]) as usize;
        let start = index + 5;
        let end = start.saturating_add(length);
        if end > data.len() {
            break;
        }
        if flags & 0x80 != 0 {
            if let Ok(text) = std::str::from_utf8(&data[start..end]) {
                for line in text.lines() {
                    if line.is_empty() {
                        continue;
                    }
                    if let Some((k, v)) = line.split_once(':') {
                        fields.insert(
                            k.trim().to_ascii_lowercase(),
                            v.trim().to_string(),
                        );
                    }
                }
            }
        }
        index = end;
    }
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
    while index < data.len() {
        let field_start = index;
        let Some(key) = read_varint(data, &mut index) else {
            index = field_start + 1;
            continue;
        };
        if key == 0 {
            index = field_start + 1;
            continue;
        }
        let field_number = key >> 3;
        let wire_type = key & 0x07;
        let mut field_path = path.to_vec();
        field_path.push(field_number);

        match wire_type {
            0 => {
                if let Some(value) = read_varint(data, &mut index) {
                    scan.varints.push(VarintField {
                        path: field_path,
                        value,
                    });
                } else {
                    index = field_start + 1;
                }
            }
            1 => {
                if index + 8 > data.len() {
                    break;
                }
                index += 8;
            }
            2 => {
                let Some(length) = read_varint(data, &mut index) else {
                    index = field_start + 1;
                    continue;
                };
                let length = length as usize;
                if length > data.len().saturating_sub(index) {
                    index = field_start + 1;
                    continue;
                }
                let start = index;
                let end = index + length;
                if depth < 4 {
                    let nested = scan_protobuf(&data[start..end], depth + 1, &field_path);
                    scan.merge(nested);
                }
                index = end;
            }
            5 => {
                if index + 4 > data.len() {
                    break;
                }
                let bits = u32::from_le_bytes([
                    data[index],
                    data[index + 1],
                    data[index + 2],
                    data[index + 3],
                ]);
                scan.fixed32.push(Fixed32Field {
                    path: field_path,
                    value: f32::from_bits(bits),
                    order,
                });
                order += 1;
                index += 4;
            }
            _ => {
                index = field_start + 1;
            }
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
/// Claude/Codex: `5h 95% · 7d 66%`
/// Grok: `mo 72%` (monthly credits remaining)
pub fn format_remaining_short(usage: &OauthUsage) -> String {
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
    fn codex_primary_secondary() {
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
