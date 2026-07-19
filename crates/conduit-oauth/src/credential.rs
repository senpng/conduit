//! Unified OAuth credential bundle stored in the secret backend.

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::OAuthError;

/// Provider discriminant for OAuth credentials.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OAuthProviderKind {
    Claude,
    Codex,
    /// xAI / Grok CLI OAuth
    #[serde(alias = "xai", alias = "grok")]
    Xai,
}

impl OAuthProviderKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
            Self::Xai => "xai",
        }
    }

    /// Provider kind string stored on the `providers` table row.
    pub fn provider_kind_str(self) -> &'static str {
        match self {
            Self::Claude => "claude-oauth",
            Self::Codex => "codex-oauth",
            Self::Xai => "grok-oauth",
        }
    }

    /// Default upstream base URL stored after OAuth login.
    ///
    /// For xAI this is the **official** API base (CLIProxyAPI `DefaultAPIBaseURL`
    /// parity). Chat traffic still rewrites empty/official bases to
    /// `cli-chat-proxy` at request time via [`crate::providers::grok::resolve_oauth_chat_base`].
    pub fn default_base_url(self) -> &'static str {
        match self {
            Self::Claude => "https://api.anthropic.com",
            Self::Codex => "https://chatgpt.com/backend-api/codex",
            Self::Xai => "https://api.x.ai/v1",
        }
    }

    pub fn parse(s: &str) -> Result<Self, OAuthError> {
        match s.trim().to_ascii_lowercase().as_str() {
            "claude" | "claude-oauth" | "anthropic-oauth" => Ok(Self::Claude),
            "codex" | "codex-oauth" => Ok(Self::Codex),
            "grok" | "grok-oauth" | "xai" | "xai-oauth" => Ok(Self::Xai),
            other => Err(OAuthError::UnsupportedKind(other.to_string())),
        }
    }
}

impl std::fmt::Display for OAuthProviderKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// OAuth credential bundle persisted as JSON in the secret backend.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OAuthCredential {
    /// `"claude" | "codex" | "xai"`
    #[serde(rename = "type")]
    pub provider_type: String,

    #[serde(default = "default_auth_kind")]
    pub auth_kind: String,

    pub access_token: String,

    #[serde(default)]
    pub refresh_token: String,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id_token: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token_type: Option<String>,

    /// RFC3339 expiry of the access token (`expired` for CLIProxyAPI compat).
    #[serde(default, rename = "expired", alias = "expire")]
    pub expired: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_refresh: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,

    /// ChatGPT account id (Codex).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account_id: Option<String>,

    /// ChatGPT plan type from id_token (e.g. `plus`, `team`, `k12`) — Codex multi-auth naming.
    #[serde(default, skip_serializing_if = "Option::is_none", alias = "plan")]
    pub plan_type: Option<String>,

    /// Claude organization UUID (token response `organization.uuid`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub organization_id: Option<String>,

    /// Claude organization name (token response `organization.name`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub organization_name: Option<String>,

    /// xAI subject claim.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sub: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,

    /// Grok: discovered token endpoint for refresh.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token_endpoint: Option<String>,

    /// Per-credential HTTP(S)/SOCKS proxy (CLIProxyAPI `auth.ProxyURL`).
    /// Overrides daemon config / env when set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proxy_url: Option<String>,

    /// xAI: when true, chat uses the official API base instead of cli-chat-proxy
    /// (CLIProxyAPI `using_api` attribute). Default false for OAuth subscription.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub using_api: Option<bool>,

    /// Preserve unknown fields on round-trip.
    #[serde(flatten)]
    pub extra: std::collections::HashMap<String, Value>,
}

fn default_auth_kind() -> String {
    "oauth".into()
}

impl OAuthCredential {
    pub fn kind(&self) -> Result<OAuthProviderKind, OAuthError> {
        OAuthProviderKind::parse(&self.provider_type)
    }

    pub fn is_oauth(&self) -> bool {
        self.auth_kind.eq_ignore_ascii_case("oauth")
            || !self.refresh_token.is_empty()
            || self.provider_type.eq_ignore_ascii_case("claude")
            || self.provider_type.eq_ignore_ascii_case("codex")
            || self.provider_type.eq_ignore_ascii_case("xai")
    }

    pub fn expired_at(&self) -> Option<DateTime<Utc>> {
        self.expired
            .as_deref()
            .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
            .map(|dt| dt.with_timezone(&Utc))
    }

    /// True when access token is missing expiry or expires within `lead`.
    pub fn needs_refresh(&self, lead: Duration) -> bool {
        if self.access_token.is_empty() {
            return true;
        }
        match self.expired_at() {
            None => false, // unknown expiry: use as-is until 401
            Some(exp) => Utc::now() + lead >= exp,
        }
    }

    pub fn to_json_bytes(&self) -> Result<Vec<u8>, OAuthError> {
        // Drop known top-level keys from `extra` so `#[serde(flatten)]` cannot
        // emit duplicate JSON keys (breaks deserialize + OAuth resolve).
        let mut cleaned = self.clone();
        cleaned.sanitize_extra();
        serde_json::to_vec(&cleaned).map_err(|e| OAuthError::Serialization(e.to_string()))
    }

    pub fn from_json_bytes(bytes: &[u8]) -> Result<Self, OAuthError> {
        // Prefer Value intermediate: historical blobs may contain duplicate keys
        // (e.g. `plan_type` both top-level and in flattened extra). Parsing to
        // `Value` keeps the last occurrence; direct struct deserialize errors.
        let v: Value = serde_json::from_slice(bytes)
            .map_err(|e| OAuthError::Serialization(e.to_string()))?;
        let mut cred: Self = serde_json::from_value(v)
            .map_err(|e| OAuthError::Serialization(e.to_string()))?;
        cred.sanitize_extra();
        Ok(cred)
    }

    /// Try parse as OAuth JSON; returns `None` if this looks like a raw API key.
    pub fn try_parse_secret(bytes: &[u8]) -> Option<Self> {
        let s = std::str::from_utf8(bytes).ok()?.trim();
        if !s.starts_with('{') {
            return None;
        }
        let cred = Self::from_json_bytes(s.as_bytes()).ok()?;
        if cred.access_token.is_empty() && cred.refresh_token.is_empty() {
            return None;
        }
        Some(cred)
    }

    /// Keys owned by typed fields — must not also live in `extra` under flatten.
    const RESERVED_EXTRA_KEYS: &'static [&'static str] = &[
        "type",
        "auth_kind",
        "access_token",
        "refresh_token",
        "id_token",
        "token_type",
        "expired",
        "expire",
        "last_refresh",
        "email",
        "account_id",
        "plan_type",
        "plan",
        "organization_id",
        "organization_name",
        "sub",
        "base_url",
        "token_endpoint",
        "proxy_url",
        "using_api",
    ];

    /// Promote known values out of `extra`, then drop reserved keys so flatten
    /// cannot emit duplicate JSON keys on the next serialize.
    fn sanitize_extra(&mut self) {
        if self.plan_type.is_none() {
            if let Some(s) = self
                .extra
                .get("plan_type")
                .or_else(|| self.extra.get("plan"))
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|s| !s.is_empty())
            {
                self.plan_type = Some(s.to_string());
            }
        }
        if self.email.is_none() {
            if let Some(s) = self
                .extra
                .get("email")
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|s| !s.is_empty())
            {
                self.email = Some(s.to_string());
            }
        }
        if self.account_id.is_none() {
            if let Some(s) = self
                .extra
                .get("account_id")
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|s| !s.is_empty())
            {
                self.account_id = Some(s.to_string());
            }
        }
        if self.proxy_url.is_none() {
            if let Some(s) = self
                .extra
                .get("proxy_url")
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|s| !s.is_empty())
            {
                self.proxy_url = Some(s.to_string());
            }
        }
        if self.using_api.is_none() {
            match self.extra.get("using_api") {
                Some(Value::Bool(b)) => self.using_api = Some(*b),
                Some(Value::String(s)) => {
                    let s = s.trim();
                    if s.eq_ignore_ascii_case("true") || s == "1" {
                        self.using_api = Some(true);
                    } else if s.eq_ignore_ascii_case("false") || s == "0" {
                        self.using_api = Some(false);
                    }
                }
                Some(Value::Number(n)) if n.as_i64() == Some(1) => self.using_api = Some(true),
                Some(Value::Number(n)) if n.as_i64() == Some(0) => self.using_api = Some(false),
                _ => {}
            }
        }
        for k in Self::RESERVED_EXTRA_KEYS {
            self.extra.remove(*k);
        }
    }

    /// Effective per-credential proxy override (field or `extra.proxy_url`).
    pub fn proxy_url_override(&self) -> Option<&str> {
        self.proxy_url
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .or_else(|| {
                self.extra
                    .get("proxy_url")
                    .and_then(|v| v.as_str())
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
            })
    }

    /// CLIProxyAPI `using_api` semantics (default false for OAuth subscription).
    pub fn using_api(&self) -> bool {
        if let Some(v) = self.using_api {
            return v;
        }
        match self.extra.get("using_api") {
            Some(Value::Bool(b)) => *b,
            Some(Value::String(s)) => {
                let s = s.trim();
                s.eq_ignore_ascii_case("true") || s == "1"
            }
            Some(Value::Number(n)) => n.as_i64() == Some(1),
            _ => false,
        }
    }

    /// Effective plan type (field or `extra.plan_type`).
    pub fn plan_type_str(&self) -> Option<&str> {
        self.plan_type
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .or_else(|| {
                self.extra
                    .get("plan_type")
                    .or_else(|| self.extra.get("plan"))
                    .and_then(|v| v.as_str())
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
            })
    }
}

/// Result of resolving a provider secret for an upstream call.
#[derive(Debug, Clone)]
pub struct ResolvedCredential {
    pub access_token: secrecy::SecretString,
    pub auth_mode: AuthMode,
    /// Extra headers to inject (e.g. Chatgpt-Account-Id, Anthropic-Beta).
    pub extra_headers: Vec<(String, String)>,
    /// Email / account label for logging (non-secret).
    pub label: Option<String>,
    /// xAI OAuth: use official API instead of cli-chat-proxy (CLIProxyAPI `using_api`).
    pub using_api: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuthMode {
    ApiKey,
    OAuth(OAuthProviderKind),
}

/// Anthropic beta header values required for Claude OAuth / Claude Code API.
/// Kept in sync with CLIProxyAPI `applyClaudeHeaders` base betas (relay also sets these per-request).
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

/// Build extra headers for a resolved OAuth credential.
pub fn oauth_extra_headers(
    kind: OAuthProviderKind,
    cred: &OAuthCredential,
) -> Vec<(String, String)> {
    match kind {
        // Claude OAuth fingerprint headers (betas, version, UA, session, stainless)
        // are applied per-request in `conduit_upstream::claude_oauth` so they are
        // not duplicated via CompositeAuth (reqwest `header` appends).
        OAuthProviderKind::Claude => vec![],
        OAuthProviderKind::Codex => {
            // CLIProxyAPI `applyCodexHeaders` defaults (ChatGPT-account OAuth path).
            let mut h = vec![
                ("Originator".into(), "codex-tui".into()),
                (
                    "User-Agent".into(),
                    "codex-tui/0.135.0 (Mac OS 26.5.0; arm64) iTerm.app/3.6.10 (codex-tui; 0.135.0)"
                        .into(),
                ),
            ];
            if let Some(ref aid) = cred.account_id {
                if !aid.is_empty() {
                    h.push(("Chatgpt-Account-Id".into(), aid.clone()));
                }
            }
            h
        }
        OAuthProviderKind::Xai => {
            // CLIProxyAPI `applyXAIChatHeaders`: CLI identity headers only when
            // not using_api and the resolved chat base is cli-chat-proxy.
            let using_api = cred.using_api();
            let base = crate::providers::grok::resolve_oauth_chat_base(
                cred.base_url.as_deref(),
                using_api,
            );
            if !using_api && crate::providers::grok::is_cli_chat_proxy_base(&base) {
                crate::providers::grok::cli_proxy_headers()
            } else {
                vec![]
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_credential_json() {
        let c = OAuthCredential {
            provider_type: "claude".into(),
            auth_kind: "oauth".into(),
            access_token: "at".into(),
            refresh_token: "rt".into(),
            id_token: None,
            token_type: Some("Bearer".into()),
            expired: Some("2030-01-01T00:00:00Z".into()),
            last_refresh: None,
            email: Some("a@b.com".into()),
            account_id: None,
            plan_type: None,
            organization_id: None,
            organization_name: None,
            sub: None,
            base_url: None,
            token_endpoint: None,
            proxy_url: None,
            using_api: None,
            extra: Default::default(),
        };
        let bytes = c.to_json_bytes().unwrap();
        let back = OAuthCredential::from_json_bytes(&bytes).unwrap();
        assert_eq!(back.access_token, "at");
        assert_eq!(back.kind().unwrap(), OAuthProviderKind::Claude);
    }

    #[test]
    fn try_parse_tolerates_duplicate_plan_type_json() {
        // Historical bug: plan_type written both as typed field and in flatten extra.
        // Raw JSON with the key twice fails direct serde; Value intermediate recovers.
        let raw = br#"{"type":"codex","auth_kind":"oauth","access_token":"at-1","refresh_token":"rt-1","plan_type":"prolite","plan_type":"prolite","email":"u@x.com","account_id":"acc-1"}"#;
        let cred = OAuthCredential::try_parse_secret(raw).expect("should parse");
        assert_eq!(cred.access_token, "at-1");
        assert_eq!(cred.plan_type.as_deref(), Some("prolite"));
        assert_eq!(cred.account_id.as_deref(), Some("acc-1"));
        // Re-serialize must be clean (single plan_type).
        let bytes = cred.to_json_bytes().unwrap();
        let s = std::str::from_utf8(&bytes).unwrap();
        assert_eq!(s.matches("\"plan_type\"").count(), 1);
        let back = OAuthCredential::from_json_bytes(&bytes).unwrap();
        assert_eq!(back.plan_type.as_deref(), Some("prolite"));
        assert!(!back.extra.contains_key("plan_type"));
    }

    #[test]
    fn to_json_strips_reserved_keys_from_extra() {
        let mut c = OAuthCredential {
            provider_type: "codex".into(),
            auth_kind: "oauth".into(),
            access_token: "at".into(),
            refresh_token: "rt".into(),
            id_token: None,
            token_type: None,
            expired: None,
            last_refresh: None,
            email: None,
            account_id: None,
            plan_type: Some("plus".into()),
            organization_id: None,
            organization_name: None,
            sub: None,
            base_url: None,
            token_endpoint: None,
            proxy_url: None,
            using_api: None,
            extra: Default::default(),
        };
        c.extra
            .insert("plan_type".into(), Value::String("plus".into()));
        c.extra
            .insert("chatgpt_user_id".into(), Value::String("u1".into()));
        let bytes = c.to_json_bytes().unwrap();
        let s = std::str::from_utf8(&bytes).unwrap();
        assert_eq!(s.matches("\"plan_type\"").count(), 1);
        let back = OAuthCredential::from_json_bytes(&bytes).unwrap();
        assert_eq!(back.plan_type.as_deref(), Some("plus"));
        assert_eq!(
            back.extra.get("chatgpt_user_id").and_then(|v| v.as_str()),
            Some("u1")
        );
    }

    #[test]
    fn try_parse_raw_api_key_returns_none() {
        assert!(OAuthCredential::try_parse_secret(b"sk-live-abc").is_none());
    }

    #[test]
    fn using_api_from_field_and_extra() {
        let mut c = OAuthCredential {
            provider_type: "xai".into(),
            auth_kind: "oauth".into(),
            access_token: "t".into(),
            refresh_token: "r".into(),
            id_token: None,
            token_type: None,
            expired: None,
            last_refresh: None,
            email: None,
            account_id: None,
            plan_type: None,
            organization_id: None,
            organization_name: None,
            sub: None,
            base_url: None,
            token_endpoint: None,
            proxy_url: None,
            using_api: Some(true),
            extra: Default::default(),
        };
        assert!(c.using_api());
        c.using_api = None;
        assert!(!c.using_api());
        c.extra
            .insert("using_api".into(), Value::String("true".into()));
        assert!(c.using_api());
    }

    #[test]
    fn needs_refresh_near_expiry() {
        let mut c = OAuthCredential {
            provider_type: "xai".into(),
            auth_kind: "oauth".into(),
            access_token: "t".into(),
            refresh_token: "r".into(),
            id_token: None,
            token_type: None,
            expired: Some((Utc::now() + Duration::minutes(2)).to_rfc3339()),
            last_refresh: None,
            email: None,
            account_id: None,
            plan_type: None,
            organization_id: None,
            organization_name: None,
            sub: None,
            base_url: None,
            token_endpoint: None,
            proxy_url: None,
            using_api: None,
            extra: Default::default(),
        };
        assert!(c.needs_refresh(Duration::minutes(5)));
        c.expired = Some((Utc::now() + Duration::hours(1)).to_rfc3339());
        assert!(!c.needs_refresh(Duration::minutes(5)));
    }
}
