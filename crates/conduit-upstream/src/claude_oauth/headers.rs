//! Outbound headers matching CLIProxyAPI `applyClaudeHeaders` for OAuth.

use uuid::Uuid;

use super::{
    device_profile::{resolve_device_profile, ClaudeHeaderDefaults, DEFAULT_TIMEOUT},
    options::ClaudeOAuthRelayOptions,
    session::cached_session_id,
};

/// Full Anthropic-Beta list used by CLIProxyAPI Claude OAuth executor.
pub const CLAUDE_OAUTH_BETAS: &str = concat!(
    "claude-code-20250219,",
    "oauth-2025-04-20,",
    "interleaved-thinking-2025-05-14,",
    "context-management-2025-06-27,",
    "prompt-caching-scope-2026-01-05,",
    "structured-outputs-2025-12-15,",
    "fast-mode-2026-02-01,",
    "redact-thinking-2026-02-12,",
    "token-efficient-tools-2026-03-28"
);

fn client_header<'a>(client: &'a [(String, String)], name: &str) -> Option<&'a str> {
    client
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case(name))
        .map(|(_, v)| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
}

/// Merge base betas + client Anthropic-Beta + body extras (CLIProxyAPI order).
pub fn merge_anthropic_betas(client_headers: &[(String, String)], extra: &[String]) -> String {
    // Prefer client Anthropic-Beta when present (then ensure oauth + interleaved).
    let mut base = if let Some(client_beta) = client_header(client_headers, "Anthropic-Beta") {
        let mut b = client_beta.to_string();
        if !b.contains("oauth") {
            b.push_str(",oauth-2025-04-20");
        }
        if !b.contains("interleaved-thinking") {
            b.push_str(",interleaved-thinking-2025-05-14");
        }
        b
    } else {
        CLAUDE_OAUTH_BETAS.to_string()
    };

    let mut seen: std::collections::HashSet<String> = base
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .collect();

    for beta in extra {
        let b = beta.trim();
        if b.is_empty() || seen.contains(b) {
            continue;
        }
        seen.insert(b.to_string());
        base.push(',');
        base.push_str(b);
    }
    base
}

/// Build the full Claude Code OAuth header set (CLIProxyAPI parity).
/// Does **not** include Authorization — caller adds Bearer.
pub fn build_claude_oauth_headers(
    access_token: &str,
    stream: bool,
    extra_betas: &[String],
    opts: &ClaudeOAuthRelayOptions,
) -> Vec<(String, String)> {
    let defaults: &ClaudeHeaderDefaults = &opts.header_defaults;
    let profile = resolve_device_profile(access_token, &opts.client_headers, defaults);
    let session_id = cached_session_id(access_token);
    let betas = merge_anthropic_betas(&opts.client_headers, extra_betas);
    let timeout = defaults
        .timeout
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(DEFAULT_TIMEOUT);

    // Anthropic-Version: prefer client if present (EnsureHeader parity).
    let anthropic_version =
        client_header(&opts.client_headers, "Anthropic-Version").unwrap_or("2023-06-01");

    let mut headers = vec![
        ("Content-Type".into(), "application/json".into()),
        ("Anthropic-Beta".into(), betas),
        ("Anthropic-Version".into(), anthropic_version.into()),
        ("X-App".into(), "cli".into()),
        ("X-Stainless-Retry-Count".into(), "0".into()),
        ("X-Stainless-Runtime".into(), "node".into()),
        ("X-Stainless-Lang".into(), "js".into()),
        ("X-Stainless-Timeout".into(), timeout.into()),
        (
            "X-Stainless-Package-Version".into(),
            profile.package_version.clone(),
        ),
        (
            "X-Stainless-Runtime-Version".into(),
            profile.runtime_version.clone(),
        ),
        ("X-Stainless-Os".into(), profile.os.clone()),
        ("X-Stainless-Arch".into(), profile.arch.clone()),
        ("X-Claude-Code-Session-Id".into(), session_id),
        ("x-client-request-id".into(), Uuid::new_v4().to_string()),
        ("User-Agent".into(), profile.user_agent.clone()),
        ("Connection".into(), "keep-alive".into()),
    ];

    // Real Claude Code CLI does NOT send Anthropic-Dangerous-Direct-Browser-Access on OAuth.

    if stream {
        headers.push(("Accept".into(), "text/event-stream".into()));
        headers.push(("Accept-Encoding".into(), "identity".into()));
    } else {
        headers.push(("Accept".into(), "application/json".into()));
        // gzip/br that Chrome client can decode; omit zstd for portability.
        headers.push(("Accept-Encoding".into(), "gzip, deflate, br".into()));
    }

    headers
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::claude_oauth::device_profile::DEFAULT_USER_AGENT;

    #[test]
    fn merges_extra_betas_without_dup() {
        let merged =
            merge_anthropic_betas(&[], &["oauth-2025-04-20".into(), "my-custom-beta".into()]);
        assert!(merged.contains("my-custom-beta"));
        assert_eq!(
            merged.matches("oauth-2025-04-20").count(),
            1,
            "oauth beta should appear once: {merged}"
        );
    }

    #[test]
    fn stream_headers_force_identity_encoding() {
        let h = build_claude_oauth_headers(
            "sk-ant-oat-x",
            true,
            &[],
            &ClaudeOAuthRelayOptions::default(),
        );
        let ae = h
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case("Accept-Encoding"))
            .map(|(_, v)| v.as_str())
            .unwrap();
        assert_eq!(ae, "identity");
        assert!(h
            .iter()
            .any(|(k, v)| k == "User-Agent" && v == DEFAULT_USER_AGENT));
    }

    #[test]
    fn passthrough_claude_cli_user_agent_legacy() {
        let opts = ClaudeOAuthRelayOptions {
            client_headers: vec![(
                "User-Agent".into(),
                "claude-cli/2.2.0 (external, vscode)".into(),
            )],
            ..Default::default()
        };
        let h = build_claude_oauth_headers("tok", false, &[], &opts);
        let ua = h
            .iter()
            .find(|(k, _)| k == "User-Agent")
            .map(|(_, v)| v.as_str())
            .unwrap();
        assert_eq!(ua, "claude-cli/2.2.0 (external, vscode)");
    }

    #[test]
    fn config_override_user_agent() {
        let opts = ClaudeOAuthRelayOptions {
            header_defaults: ClaudeHeaderDefaults {
                user_agent: Some("claude-cli/2.1.70 (external, cli)".into()),
                ..Default::default()
            },
            client_headers: vec![("User-Agent".into(), "curl/8".into())],
            ..Default::default()
        };
        let h = build_claude_oauth_headers("tok", false, &[], &opts);
        let ua = h
            .iter()
            .find(|(k, _)| k == "User-Agent")
            .map(|(_, v)| v.as_str())
            .unwrap();
        assert_eq!(ua, "claude-cli/2.1.70 (external, cli)");
    }
}
