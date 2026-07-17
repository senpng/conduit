//! Cloak / header options (CLIProxyAPI ClaudeKey.Cloak + ClaudeHeaderDefaults).

use super::device_profile::{parse_entrypoint_from_ua, ClaudeHeaderDefaults};

/// Full relay options — cloak + device profile + client request headers.
#[derive(Debug, Clone)]
pub struct ClaudeOAuthRelayOptions {
    /// `auto` | `always` | `never` — default `auto`.
    pub cloak_mode: String,
    /// Keep original system in system[] instead of moving to user (strict).
    pub strict_mode: bool,
    /// Zero-width-space obfuscation targets.
    pub sensitive_words: Vec<String>,
    /// Cache fake user_id per access token (default true).
    pub cache_user_id: bool,
    /// Downstream request headers (User-Agent, Stainless, Anthropic-Beta, …).
    pub client_headers: Vec<(String, String)>,
    /// Config overrides for baseline fingerprint (CLIProxyAPI ClaudeHeaderDefaults).
    pub header_defaults: ClaudeHeaderDefaults,
    /// Optional override for billing header version; empty = derive from UA.
    pub claude_version: String,
    /// Optional override for billing entrypoint; empty = parse from client UA.
    pub entrypoint: String,
}

impl Default for ClaudeOAuthRelayOptions {
    fn default() -> Self {
        Self {
            cloak_mode: "auto".into(),
            strict_mode: false,
            sensitive_words: Vec::new(),
            cache_user_id: true,
            client_headers: Vec::new(),
            header_defaults: ClaudeHeaderDefaults::default(),
            claude_version: String::new(),
            entrypoint: String::new(),
        }
    }
}

impl ClaudeOAuthRelayOptions {
    pub fn client_user_agent(&self) -> &str {
        self.client_headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case("User-Agent"))
            .map(|(_, v)| v.as_str())
            .unwrap_or("")
    }

    /// Effective entrypoint for billing header.
    pub fn effective_entrypoint(&self) -> String {
        if !self.entrypoint.trim().is_empty() {
            return self.entrypoint.trim().to_string();
        }
        let ua = self.client_user_agent();
        if ua.is_empty() {
            "cli".into()
        } else {
            parse_entrypoint_from_ua(ua)
        }
    }

    /// Build options from downstream HTTP headers (gateway ingress).
    pub fn from_client_headers(headers: Vec<(String, String)>) -> Self {
        Self {
            client_headers: headers,
            ..Default::default()
        }
    }
}

/// CLIProxyAPI `ShouldCloak`: always / never / auto (cloak unless UA is claude-cli).
pub fn should_cloak(cloak_mode: &str, user_agent: &str) -> bool {
    match cloak_mode.to_ascii_lowercase().as_str() {
        "always" => true,
        "never" => false,
        _ => !user_agent.starts_with("claude-cli"),
    }
}
