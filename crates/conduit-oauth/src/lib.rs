//! OAuth2 support for Claude (Anthropic), Codex (OpenAI/ChatGPT), and Grok (xAI).
//!
//! - Claude / Codex: authorization code + PKCE with fixed localhost redirect ports
//! - Grok: device authorization grant (RFC 8628) via OIDC discovery
//!
//! Credentials are stored as JSON [`OAuthCredential`] blobs (typically in
//! `conduit-secret` under scope `upstream_key`).

pub mod credential;
pub mod error;
pub mod pkce;
pub mod providers;
pub mod refresh;
pub mod resolver;
pub mod session;

pub use credential::{
    oauth_extra_headers, AuthMode, OAuthCredential, OAuthProviderKind, ResolvedCredential,
    CLAUDE_OAUTH_BETAS,
};
pub use error::OAuthError;
pub use pkce::{generate_pkce, generate_state, PkceCodes};
pub use providers::{
    grok::{
        cli_proxy_headers, resolve_oauth_chat_base, CLI_CHAT_PROXY_BASE, CLI_CLIENT_VERSION,
        DEFAULT_API_BASE as GROK_OFFICIAL_API_BASE,
    },
    ClaudeOAuth, CodexOAuth, GrokOAuth,
};
pub use refresh::RefreshCoordinator;
pub use resolver::{CredentialResolver, SecretStore};
pub use session::{OAuthSession, SessionStatus, SessionStore, SessionView, SESSION_TTL};

/// Metadata for admin UI listing.
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
