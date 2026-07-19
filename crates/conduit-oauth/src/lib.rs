//! OAuth2 support for Claude (Anthropic), Codex (OpenAI/ChatGPT), and Grok (xAI).
//!
//! - Claude / Codex: authorization code + PKCE with fixed localhost redirect ports
//! - Claude token exchange: Chrome TLS fingerprint (CLIProxyAPI `HelloChrome_Auto`)
//! - Claude / Codex refresh: up to 3 retries (Claude also has 429 Retry-After block)
//! - Grok: device authorization grant (RFC 8628) via OIDC discovery
//! - Proxy: credential `proxy_url` → `CONDUIT_PROXY_URL` → daemon config →
//!   `HTTP(S)_PROXY` / `ALL_PROXY` (SOCKS supported); bypass `NO_PROXY`
//! - Grok `using_api`: official API vs cli-chat-proxy (CLIProxyAPI parity)
//! - Remaining usage probe ([`fetch_oauth_usage`]): Claude `/api/oauth/usage`,
//!   Codex `wham/usage`, Grok grok.com billing gRPC-web (CodexBar parity)
//!
//! Credentials are stored as JSON [`OAuthCredential`] blobs (typically in
//! `conduit-secret` under scope `upstream_key`).

pub mod credential;
pub mod error;
pub mod pkce;
pub mod providers;
pub mod proxy;
pub mod refresh;
pub mod resolver;
pub mod session;
pub mod usage;

pub use credential::{
    oauth_extra_headers, AuthMode, OAuthCredential, OAuthProviderKind, ResolvedCredential,
    CLAUDE_OAUTH_BETAS,
};
pub use error::OAuthError;
pub use pkce::{generate_pkce, generate_state, PkceCodes};
pub use providers::{
    grok::{
        cli_proxy_headers, is_cli_chat_proxy_base, resolve_oauth_chat_base, CLI_CHAT_PROXY_BASE,
        CLI_CLIENT_VERSION, DEFAULT_API_BASE as GROK_OFFICIAL_API_BASE,
    },
    codex::{
        display_provider_name as codex_display_name, stable_provider_id as codex_stable_provider_id,
        CodexJwtIdentity,
    },
    ClaudeOAuth, CodexOAuth, GrokOAuth,
};
pub use proxy::{env_no_proxy, env_proxy_url, resolve_effective_proxy};
pub use refresh::RefreshCoordinator;
pub use resolver::{CredentialResolver, SecretStore};
pub use session::{OAuthSession, SessionStatus, SessionStore, SessionView, SESSION_TTL};
pub use usage::{
    fetch_oauth_usage, format_remaining_short, is_billing_source, parse_claude_usage,
    parse_codex_usage, parse_grok_billing_protobuf, OauthUsage, UsageWindow,
};

/// Metadata for console UI listing.
#[derive(Debug, Clone, serde::Serialize)]
pub struct OAuthProviderMeta {
    pub kind: &'static str,
    pub display_name: &'static str,
    pub flow: &'static str,
    pub default_base_url: &'static str,
    pub callback_port: Option<u16>,
}

pub fn supported_providers() -> Vec<OAuthProviderMeta> {
    vec![
        OAuthProviderMeta {
            kind: "claude",
            display_name: "Claude (Anthropic OAuth)",
            flow: "authorization_code_pkce",
            default_base_url: OAuthProviderKind::Claude.default_base_url(),
            callback_port: Some(providers::claude::CALLBACK_PORT),
        },
        OAuthProviderMeta {
            kind: "codex",
            display_name: "Codex (ChatGPT OAuth)",
            flow: "authorization_code_pkce",
            default_base_url: OAuthProviderKind::Codex.default_base_url(),
            callback_port: Some(providers::codex::CALLBACK_PORT),
        },
        OAuthProviderMeta {
            kind: "grok",
            display_name: "Grok (xAI Device Code)",
            flow: "device_code",
            default_base_url: OAuthProviderKind::Xai.default_base_url(),
            callback_port: None,
        },
    ]
}
