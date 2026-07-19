//! Proactive subscription quota probe for OAuth providers.
//!
//! Claude: `GET https://api.anthropic.com/api/oauth/usage`
//! Codex:  `GET https://chatgpt.com/backend-api/wham/usage`
//!
//! Both return session (≈5h) and weekly (≈7d) utilization; we normalize to
//! **remaining percent** (0–100) for the console / TUI.

use serde_json::Value;
use secrecy::ExposeSecret;

use crate::{
    credential::{oauth_extra_headers, AuthMode, OAuthProviderKind, ResolvedCredential},
    error::OAuthError,
    proxy::apply_reqwest_proxy,
};

const CLAUDE_USAGE_URL: &str = "https://api.anthropic.com/api/oauth/usage";
const CLAUDE_OAUTH_BETA: &str = "oauth-2025-04-20";
const CLAUDE_ANTHROPIC_VERSION: &str = "2023-06-01";
const CODEX_USAGE_URL: &str = "https://chatgpt.com/backend-api/wham/usage";

/// One rolling usage window (session / weekly).
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
    pub session: Option<UsageWindow>,
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
        OAuthProviderKind::Xai => Err(OAuthError::UnsupportedKind(
            "grok oauth has no public remaining-usage API yet".into(),
        )),
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
    if !matches!(resolved.auth_mode, AuthMode::OAuth(OAuthProviderKind::Claude)) {
        // Still ok if OAuth kind mismatch in auth_mode but token present.
    }
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
    // Also try headers from oauth_extra_headers path if resolver put them there.
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

/// Parse Claude `/api/oauth/usage` JSON into windows.
pub fn parse_claude_usage(body: &str) -> Result<OauthUsage, OAuthError> {
    let v: Value = serde_json::from_str(body)
        .map_err(|e| OAuthError::Provider(format!("claude usage json: {e}")))?;
    let session = window_from_utilization(v.get("five_hour"));
    let weekly = window_from_utilization(v.get("seven_day"));
    if session.is_none() && weekly.is_none() {
        // Richer body may only have `limits` array — pick session / weekly_all.
        let mut session = None;
        let mut weekly = None;
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
                    "session" if session.is_none() => session = win,
                    "weekly_all" | "weekly" if weekly.is_none() => weekly = win,
                    _ => {}
                }
            }
        }
        if session.is_none() && weekly.is_none() {
            return Err(OAuthError::Provider(
                "claude usage: no five_hour/seven_day windows".into(),
            ));
        }
        return Ok(OauthUsage {
            provider_kind: OAuthProviderKind::Claude,
            session,
            weekly,
            source: "oauth_usage_api",
            raw_excerpt: body.chars().take(256).collect(),
        });
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
        .or_else(|| node.get("utilization").and_then(|x| x.as_i64()).map(|i| i as f64))
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

/// Compact one-line summary for list columns, e.g. `5h 95% · 7d 66%`.
pub fn format_remaining_short(usage: &OauthUsage) -> String {
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

/// Build extra headers for a usage probe from a raw credential (Codex account id).
pub fn usage_headers_for_cred(
    kind: OAuthProviderKind,
    cred: &crate::credential::OAuthCredential,
) -> Vec<(String, String)> {
    oauth_extra_headers(kind, cred)
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
}
