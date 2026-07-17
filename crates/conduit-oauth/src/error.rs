use thiserror::Error;

/// Errors from OAuth flows, token exchange, and refresh.
#[derive(Debug, Error)]
pub enum OAuthError {
    #[error("invalid state: {0}")]
    InvalidState(String),

    #[error("pkce error: {0}")]
    Pkce(String),

    #[error("session not found: {0}")]
    SessionNotFound(String),

    #[error("session cancelled")]
    SessionCancelled,

    #[error("session timed out")]
    SessionTimeout,

    #[error("provider error: {0}")]
    Provider(String),

    #[error("token exchange failed ({status}): {body}")]
    TokenExchange { status: u16, body: String },

    #[error("token refresh failed ({status}): {body}")]
    TokenRefresh { status: u16, body: String },

    #[error("device authorization pending")]
    AuthorizationPending,

    #[error("device authorization denied")]
    AccessDenied,

    #[error("device code expired")]
    DeviceCodeExpired,

    #[error("network error: {0}")]
    Network(String),

    #[error("serialization error: {0}")]
    Serialization(String),

    #[error("callback port {0} is already in use")]
    PortInUse(u16),

    #[error("unsupported oauth kind: {0}")]
    UnsupportedKind(String),

    #[error("credential error: {0}")]
    Credential(String),
}

impl OAuthError {
    pub fn is_retryable_refresh(&self) -> bool {
        match self {
            Self::TokenRefresh { status, .. } => *status >= 500,
            Self::Network(_) => true,
            _ => false,
        }
    }
}
