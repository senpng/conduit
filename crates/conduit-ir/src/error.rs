use thiserror::Error;

// ---------------------------------------------------------------------------
// Provider errors — returned by upstream adapters
// ---------------------------------------------------------------------------

/// Errors returned by provider HTTP adapters when calling an upstream API.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum ProviderError {
    #[error("rate limited: {0}")]
    RateLimited(String),

    #[error("unauthorized: {0}")]
    Unauthorized(String),

    #[error("invalid request: {0}")]
    InvalidRequest(String),

    #[error("upstream 5xx: {0}")]
    Upstream5xx(String),

    #[error("network error: {0}")]
    Network(String),

    #[error("serialization error: {0}")]
    Serialization(String),

    #[error("timeout")]
    Timeout,

    #[error("context length exceeded")]
    ContextLengthExceeded,
}

impl ProviderError {
    /// Returns true for errors that are worth retrying on a different provider.
    pub fn is_retryable(&self) -> bool {
        matches!(
            self,
            ProviderError::RateLimited(_)
                | ProviderError::Upstream5xx(_)
                | ProviderError::Network(_)
                | ProviderError::Timeout
        )
    }

    /// Returns the canonical HTTP status hint, if applicable.
    pub fn http_status_hint(&self) -> Option<u16> {
        match self {
            ProviderError::RateLimited(_) => Some(429),
            ProviderError::Unauthorized(_) => Some(401),
            ProviderError::InvalidRequest(_) => Some(400),
            ProviderError::Upstream5xx(_) => Some(500),
            ProviderError::ContextLengthExceeded => Some(400),
            _ => None,
        }
    }
}

// ---------------------------------------------------------------------------
// Codec errors — returned by request/response codec conversion
// ---------------------------------------------------------------------------

/// Errors produced by codec translation (canonical ↔ provider wire format).
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum CodecError {
    #[error("unsupported field '{field}': {reason}")]
    UnsupportedField { field: String, reason: String },

    #[error("missing required field '{field}'")]
    MissingField { field: String },

    #[error("invalid value for field '{field}': {value}")]
    InvalidValue { field: String, value: String },

    #[error("serialization failed: {0}")]
    Serialization(#[from] serde_json::Error),

    #[error("provider '{provider}' does not support model '{model}'")]
    UnsupportedModel { provider: String, model: String },
}

// ---------------------------------------------------------------------------
// Secret errors — returned by secret backends
// ---------------------------------------------------------------------------

/// Errors from secret storage backends (encrypted files, etc.).
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum SecretError {
    #[error("secret '{key}' not found")]
    NotFound { key: String },

    #[error("permission denied accessing secret '{key}': {reason}")]
    PermissionDenied { key: String, reason: String },

    #[error("backend unavailable: {0}")]
    BackendUnavailable(String),

    #[error("decryption failed for secret '{key}': {reason}")]
    DecryptionFailed { key: String, reason: String },

    #[error("secret '{key}' has expired")]
    Expired { key: String },

    #[error("serialization error: {0}")]
    Serialization(String),
}

// ---------------------------------------------------------------------------
// Quota errors — rate checks / usage backend
// ---------------------------------------------------------------------------

/// Errors from quota enforcement (rate-limit checks and usage backend).
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum QuotaError {
    #[error("rate limit exceeded: {requests_per_minute} req/min")]
    RateLimitExceeded { requests_per_minute: u32 },

    #[error("token quota exceeded: used {used}, quota {quota}")]
    TokenQuotaExceeded { used: u64, quota: u64 },

    /// Rate/usage backend failed; callers must fail-closed (no unlimited allowance).
    #[error("quota backend error: {0}")]
    Backend(String),
}

// ---------------------------------------------------------------------------
// Gateway errors — internal pipeline errors
// ---------------------------------------------------------------------------

/// Errors produced within the gateway pipeline (routing, quota, secret, codec,
/// upstream, and internal logic).
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum GatewayError {
    /// Missing or invalid downstream credentials (HTTP 401).
    #[error("unauthorized: {0}")]
    Unauthorized(String),

    #[error("routing failed: {0}")]
    Routing(String),

    #[error("all upstream attempts exhausted after {attempts} tries")]
    AllAttemptsExhausted { attempts: u32 },

    #[error("quota error: {0}")]
    Quota(#[from] QuotaError),

    #[error("secret error: {0}")]
    Secret(#[from] SecretError),

    #[error("codec error: {0}")]
    Codec(#[from] CodecError),

    #[error("provider error: {0}")]
    Provider(#[from] ProviderError),

    #[error("invalid alias '{alias}': {reason}")]
    InvalidAlias { alias: String, reason: String },

    #[error("request timeout after {ms}ms")]
    RequestTimeout { ms: u64 },

    #[error("internal error: {0}")]
    Internal(String),
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_error_retryable() {
        assert!(ProviderError::RateLimited("x".into()).is_retryable());
        assert!(ProviderError::Upstream5xx("x".into()).is_retryable());
        assert!(ProviderError::Network("x".into()).is_retryable());
        assert!(ProviderError::Timeout.is_retryable());
        assert!(!ProviderError::Unauthorized("x".into()).is_retryable());
        assert!(!ProviderError::InvalidRequest("x".into()).is_retryable());
        assert!(!ProviderError::ContextLengthExceeded.is_retryable());
    }

    #[test]
    fn provider_error_http_hints() {
        assert_eq!(
            ProviderError::RateLimited("".into()).http_status_hint(),
            Some(429)
        );
        assert_eq!(
            ProviderError::Unauthorized("".into()).http_status_hint(),
            Some(401)
        );
        assert_eq!(ProviderError::Timeout.http_status_hint(), None);
    }

    #[test]
    fn gateway_error_from_quota() {
        let q = QuotaError::RateLimitExceeded {
            requests_per_minute: 60,
        };
        let g: GatewayError = q.into();
        assert!(g.to_string().contains("rate limit exceeded"));
    }

    #[test]
    fn codec_error_from_serde() {
        let serde_err = serde_json::from_str::<serde_json::Value>("{bad}").unwrap_err();
        let c: CodecError = serde_err.into();
        assert!(c.to_string().contains("serialization failed"));
    }

    #[test]
    fn quota_error_display() {
        let e = QuotaError::RateLimitExceeded {
            requests_per_minute: 60,
        };
        assert_eq!(e.to_string(), "rate limit exceeded: 60 req/min");
    }
}
