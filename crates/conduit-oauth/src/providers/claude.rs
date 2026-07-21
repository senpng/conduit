//! Anthropic Claude Code OAuth (authorization code + PKCE).
//!
//! Token exchange uses Chrome TLS fingerprint (CLIProxyAPI Anthropic auth /
//! utls `HelloChrome_Auto` on `api.anthropic.com`). Rust equivalent = latest
//! Chrome profile in current `wreq-util` (same as Messages relay client).

use std::{
    collections::HashMap,
    sync::OnceLock,
    time::{Duration as StdDuration, Instant},
};

use chrono::{Duration, Utc};
use parking_lot::Mutex;
use serde::Deserialize;
use serde_json::json;
use wreq_util::{Emulation, Profile};

use crate::{
    credential::OAuthCredential,
    error::OAuthError,
    pkce::PkceCodes,
    proxy::{apply_wreq_proxy, env_proxy_url},
};

pub const AUTH_URL: &str = "https://claude.ai/oauth/authorize";
pub const TOKEN_URL: &str = "https://api.anthropic.com/v1/oauth/token";
pub const CLIENT_ID: &str = "9d1c250a-e61b-44d9-88ed-5944d1962f5e";
pub const REDIRECT_URI: &str = "http://localhost:54545/callback";
pub const CALLBACK_PORT: u16 = 54545;
pub const SCOPE: &str =
    "user:profile user:inference user:sessions:claude_code user:mcp_servers user:file_upload";

/// Default max attempts for Claude refresh (CLIProxyAPI executor uses 3).
pub const DEFAULT_REFRESH_MAX_RETRIES: usize = 3;

const REFRESH_MIN_BACKOFF: StdDuration = StdDuration::from_secs(5);
const REFRESH_MAX_BACKOFF: StdDuration = StdDuration::from_secs(5 * 60);

/// Align with Go utls `HelloChrome_Auto`: latest Chrome preset in this wreq-util.
/// Keep in sync with `conduit_upstream::claude_oauth::http_client::chrome_auto`.
#[inline]
fn chrome_auto() -> Profile {
    Profile::Chrome149
}

fn build_chrome_client(proxy_url: Option<&str>) -> Result<wreq::Client, OAuthError> {
    // TLS/H2 fingerprint only — HTTP headers for token exchange are set explicitly.
    let builder = wreq::Client::builder().emulation(
        Emulation::builder()
            .profile(chrome_auto())
            .headers(false)
            .http2(true)
            .build(),
    );
    apply_wreq_proxy(builder, proxy_url)?
        .build()
        .map_err(|e| OAuthError::Network(format!("claude oauth client: {e}")))
}

// ── 429 refresh block (CLIProxyAPI claudeRefreshBlock) ────────────────────────

static REFRESH_BLOCK: OnceLock<Mutex<HashMap<String, Instant>>> = OnceLock::new();

fn refresh_block_map() -> &'static Mutex<HashMap<String, Instant>> {
    REFRESH_BLOCK.get_or_init(|| Mutex::new(HashMap::new()))
}

fn refresh_blocked_until(refresh_token: &str) -> Option<Instant> {
    let until = *refresh_block_map().lock().get(refresh_token)?;
    if until > Instant::now() {
        Some(until)
    } else {
        None
    }
}

fn set_refresh_blocked_until(refresh_token: &str, until: Instant) {
    refresh_block_map()
        .lock()
        .insert(refresh_token.to_string(), until);
}

fn clear_refresh_blocked(refresh_token: &str) {
    refresh_block_map().lock().remove(refresh_token);
}

fn clamp_refresh_backoff(d: StdDuration) -> StdDuration {
    d.clamp(REFRESH_MIN_BACKOFF, REFRESH_MAX_BACKOFF)
}

/// Parse `Retry-After` / `Retry-After-Ms` (CLIProxyAPI `parseClaudeRetryAfter`).
fn parse_retry_after(headers: &wreq::header::HeaderMap) -> StdDuration {
    if let Some(raw) = headers
        .get("Retry-After")
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        if let Ok(secs) = raw.parse::<u64>() {
            return clamp_refresh_backoff(StdDuration::from_secs(secs));
        }
        if let Ok(when) = httpdate_parse(raw) {
            let until = when
                .duration_since(std::time::SystemTime::now())
                .unwrap_or(REFRESH_MIN_BACKOFF);
            return clamp_refresh_backoff(until);
        }
    }
    if let Some(raw) = headers
        .get("Retry-After-Ms")
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        if let Ok(ms) = raw.parse::<u64>() {
            return clamp_refresh_backoff(StdDuration::from_millis(ms));
        }
    }
    REFRESH_MIN_BACKOFF
}

fn httpdate_parse(raw: &str) -> Result<std::time::SystemTime, ()> {
    // Minimal IMF-fix RFC 7231 date via `httpdate` if available; else fail closed.
    // Avoid new dep: try chrono RFC2822.
    chrono::DateTime::parse_from_rfc2822(raw)
        .map(|dt| {
            let ts = dt.timestamp();
            if ts <= 0 {
                return std::time::SystemTime::UNIX_EPOCH;
            }
            std::time::UNIX_EPOCH + StdDuration::from_secs(ts as u64)
        })
        .map_err(|_| ())
}

pub struct ClaudeOAuth {
    client: wreq::Client,
    token_url: String,
    client_id: String,
    redirect_uri: String,
}

impl ClaudeOAuth {
    /// Build with proxy from env (`CONDUIT_PROXY_URL` / `HTTP(S)_PROXY` / `ALL_PROXY`).
    ///
    /// When a proxy URL is set (env or explicit), construction **fails** if the
    /// proxy cannot be applied — never falls back to direct connect.
    pub fn new() -> Result<Self, OAuthError> {
        Self::with_proxy_url(env_proxy_url())
    }

    pub fn with_proxy_url(proxy_url: Option<String>) -> Result<Self, OAuthError> {
        let client = build_chrome_client(proxy_url.as_deref())?;
        Ok(Self {
            client,
            token_url: TOKEN_URL.into(),
            client_id: CLIENT_ID.into(),
            redirect_uri: REDIRECT_URI.into(),
        })
    }

    /// Test helper: override token endpoint (wiremock).
    pub fn with_token_url(mut self, url: impl Into<String>) -> Self {
        self.token_url = url.into();
        self
    }

    pub fn generate_auth_url(&self, state: &str, pkce: &PkceCodes) -> String {
        let mut url = url::Url::parse(AUTH_URL).expect("static auth url");
        {
            let mut q = url.query_pairs_mut();
            q.append_pair("code", "true");
            q.append_pair("client_id", &self.client_id);
            q.append_pair("response_type", "code");
            q.append_pair("redirect_uri", &self.redirect_uri);
            q.append_pair("scope", SCOPE);
            q.append_pair("code_challenge", &pkce.code_challenge);
            q.append_pair("code_challenge_method", "S256");
            q.append_pair("state", state);
        }
        url.to_string()
    }

    /// Claude may return `code#state` in a single `code` query param.
    pub fn parse_code_and_state(code: &str) -> (String, Option<String>) {
        let mut parts = code.splitn(2, '#');
        let c = parts.next().unwrap_or("").to_string();
        let s = parts
            .next()
            .map(|s| s.to_string())
            .filter(|s| !s.is_empty());
        (c, s)
    }

    pub async fn exchange_code(
        &self,
        code: &str,
        state: &str,
        pkce: &PkceCodes,
    ) -> Result<OAuthCredential, OAuthError> {
        let (code, state_from_code) = Self::parse_code_and_state(code);
        let state = state_from_code.as_deref().unwrap_or(state);

        let body = json!({
            "code": code,
            "state": state,
            "grant_type": "authorization_code",
            "client_id": self.client_id,
            "redirect_uri": self.redirect_uri,
            "code_verifier": pkce.code_verifier,
        });

        let resp = self
            .client
            .post(&self.token_url)
            .header("Content-Type", "application/json")
            .header("Accept", "application/json")
            .json(&body)
            .send()
            .await
            .map_err(|e| OAuthError::Network(e.to_string()))?;

        let status = resp.status().as_u16();
        let text = resp
            .text()
            .await
            .map_err(|e| OAuthError::Network(e.to_string()))?;
        if status != 200 {
            return Err(OAuthError::TokenExchange { status, body: text });
        }

        self.token_response_to_credential(&text, None)
    }

    /// Single refresh attempt with 429 blocking (CLIProxyAPI `RefreshTokens`).
    pub async fn refresh(&self, refresh_token: &str) -> Result<OAuthCredential, OAuthError> {
        if refresh_token.is_empty() {
            return Err(OAuthError::Credential("refresh token is required".into()));
        }
        if let Some(until) = refresh_blocked_until(refresh_token) {
            let secs = until.saturating_duration_since(Instant::now()).as_secs();
            return Err(OAuthError::TokenRefresh {
                status: 429,
                body: format!("refresh temporarily blocked for ~{secs}s (Retry-After)"),
            });
        }

        let body = json!({
            "client_id": self.client_id,
            "grant_type": "refresh_token",
            "refresh_token": refresh_token,
        });

        let resp = self
            .client
            .post(&self.token_url)
            .header("Content-Type", "application/json")
            .header("Accept", "application/json")
            .json(&body)
            .send()
            .await
            .map_err(|e| OAuthError::Network(e.to_string()))?;

        let status = resp.status().as_u16();
        if status == 429 {
            let backoff = parse_retry_after(resp.headers());
            set_refresh_blocked_until(refresh_token, Instant::now() + backoff);
            let text = resp
                .text()
                .await
                .unwrap_or_else(|_| "rate limited".into());
            return Err(OAuthError::TokenRefresh { status, body: text });
        }

        let text = resp
            .text()
            .await
            .map_err(|e| OAuthError::Network(e.to_string()))?;
        if status != 200 {
            return Err(OAuthError::TokenRefresh { status, body: text });
        }

        clear_refresh_blocked(refresh_token);
        self.token_response_to_credential(&text, Some(refresh_token))
    }

    /// Refresh with linear backoff retries (CLIProxyAPI `RefreshTokensWithRetry`).
    pub async fn refresh_with_retry(
        &self,
        refresh_token: &str,
        max_retries: usize,
    ) -> Result<OAuthCredential, OAuthError> {
        let max_retries = max_retries.max(1);
        let mut last_err = OAuthError::Credential("token refresh failed".into());
        for attempt in 0..max_retries {
            if attempt > 0 {
                tokio::time::sleep(StdDuration::from_secs(attempt as u64)).await;
            }
            match self.refresh(refresh_token).await {
                Ok(cred) => return Ok(cred),
                Err(e) => {
                    last_err = e;
                    if !last_err.is_retryable_refresh() {
                        break;
                    }
                }
            }
        }
        Err(last_err)
    }

    fn token_response_to_credential(
        &self,
        text: &str,
        fallback_refresh: Option<&str>,
    ) -> Result<OAuthCredential, OAuthError> {
        let tr: ClaudeTokenResponse =
            serde_json::from_str(text).map_err(|e| OAuthError::Serialization(e.to_string()))?;

        // CLIProxyAPI uses expires_in as-is (0 → immediate expiry). Missing field
        // deserializes as 0 via serde default — same as Go zero value.
        let expires_in = tr.expires_in;
        // Keep previous refresh_token when response omits/empties it (safer than
        // CLIProxyAPI which may wipe; rotation still applies when a new one is sent).
        let refresh = tr
            .refresh_token
            .filter(|s| !s.is_empty())
            .or_else(|| fallback_refresh.map(|s| s.to_string()))
            .unwrap_or_default();

        let org_id = tr
            .organization
            .as_ref()
            .and_then(|o| o.uuid.clone())
            .filter(|s| !s.is_empty());
        let org_name = tr
            .organization
            .as_ref()
            .and_then(|o| o.name.clone())
            .filter(|s| !s.is_empty());
        let account_uuid = tr
            .account
            .as_ref()
            .and_then(|a| a.uuid.clone())
            .filter(|s| !s.is_empty());
        let email = tr
            .account
            .as_ref()
            .and_then(|a| a.email_address.clone());

        let mut extra = std::collections::HashMap::new();
        if let Some(ref id) = org_id {
            extra.insert("organization_uuid".into(), serde_json::json!(id));
        }
        if let Some(ref n) = org_name {
            extra.insert("organization_name".into(), serde_json::json!(n));
        }
        if let Some(ref id) = account_uuid {
            extra.insert("account_uuid".into(), serde_json::json!(id));
        }

        Ok(OAuthCredential {
            provider_type: "claude".into(),
            auth_kind: "oauth".into(),
            access_token: tr.access_token,
            refresh_token: refresh,
            id_token: None,
            token_type: tr.token_type,
            expired: Some((Utc::now() + Duration::seconds(expires_in.max(0))).to_rfc3339()),
            last_refresh: Some(Utc::now().to_rfc3339()),
            email,
            account_id: account_uuid,
            plan_type: None,
            organization_id: org_id,
            organization_name: org_name,
            sub: None,
            base_url: Some(
                crate::credential::OAuthProviderKind::Claude
                    .default_base_url()
                    .into(),
            ),
            token_endpoint: Some(self.token_url.clone()),
            proxy_url: None,
            using_api: None,
            cloak_mode: None,
            extra,
        })
    }
}

#[derive(Debug, Deserialize)]
struct ClaudeTokenResponse {
    access_token: String,
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default)]
    token_type: Option<String>,
    #[serde(default)]
    expires_in: i64,
    #[serde(default)]
    organization: Option<ClaudeOrganization>,
    #[serde(default)]
    account: Option<ClaudeAccount>,
}

#[derive(Debug, Deserialize)]
struct ClaudeOrganization {
    #[serde(default)]
    uuid: Option<String>,
    #[serde(default)]
    name: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ClaudeAccount {
    #[serde(default)]
    uuid: Option<String>,
    #[serde(default)]
    email_address: Option<String>,
}

#[cfg(test)]
mod tests {
    use wiremock::{
        matchers::{method, path},
        Mock, MockServer, ResponseTemplate,
    };

    use super::*;
    use crate::pkce::generate_pkce;

    #[test]
    fn auth_url_contains_pkce() {
        let oauth = ClaudeOAuth::new().unwrap();
        let pkce = generate_pkce().unwrap();
        let url = oauth.generate_auth_url("mystate", &pkce);
        assert!(url.contains("code_challenge="));
        assert!(url.contains("state=mystate"));
        assert!(url.contains("client_id="));
    }

    #[test]
    fn parse_code_hash_state() {
        let (c, s) = ClaudeOAuth::parse_code_and_state("abc#xyz");
        assert_eq!(c, "abc");
        assert_eq!(s.as_deref(), Some("xyz"));
    }

    #[tokio::test]
    async fn exchange_code_success() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "access_token": "at-1",
                "refresh_token": "rt-1",
                "token_type": "Bearer",
                "expires_in": 3600,
                "account": { "email_address": "u@example.com" }
            })))
            .mount(&server)
            .await;

        let oauth = ClaudeOAuth::new().unwrap().with_token_url(format!("{}/token", server.uri()));
        let pkce = generate_pkce().unwrap();
        let cred = oauth.exchange_code("code1", "state1", &pkce).await.unwrap();
        assert_eq!(cred.access_token, "at-1");
        assert_eq!(cred.email.as_deref(), Some("u@example.com"));
        assert_eq!(cred.provider_type, "claude");
    }

    #[tokio::test]
    async fn refresh_success() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "access_token": "at-2",
                "refresh_token": "rt-2",
                "expires_in": 7200
            })))
            .mount(&server)
            .await;

        let oauth = ClaudeOAuth::new().unwrap().with_token_url(format!("{}/token", server.uri()));
        let cred = oauth.refresh("rt-old").await.unwrap();
        assert_eq!(cred.access_token, "at-2");
        assert_eq!(cred.refresh_token, "rt-2");
    }

    #[tokio::test]
    async fn refresh_preserves_old_refresh_when_response_omits() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "access_token": "at-3",
                "expires_in": 3600
            })))
            .mount(&server)
            .await;

        let oauth = ClaudeOAuth::new().unwrap().with_token_url(format!("{}/token", server.uri()));
        let cred = oauth.refresh("rt-keep").await.unwrap();
        assert_eq!(cred.refresh_token, "rt-keep");
    }

    #[tokio::test]
    async fn refresh_429_blocks_subsequent() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/token"))
            .respond_with(
                ResponseTemplate::new(429)
                    .insert_header("Retry-After", "5")
                    .set_body_string("slow down"),
            )
            .mount(&server)
            .await;

        let oauth = ClaudeOAuth::new().unwrap().with_token_url(format!("{}/token", server.uri()));
        let token = format!("rt-429-{}", ulid::Ulid::new());
        let err = oauth.refresh(&token).await.unwrap_err();
        assert_eq!(err.refresh_status(), Some(429));

        // Immediate second attempt should be blocked without hitting network.
        let err2 = oauth.refresh(&token).await.unwrap_err();
        assert_eq!(err2.refresh_status(), Some(429));
        assert!(err2.to_string().contains("blocked"));
    }

    #[tokio::test]
    async fn refresh_with_retry_stops_on_non_retryable() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/token"))
            .respond_with(ResponseTemplate::new(401).set_body_string("invalid"))
            .expect(1)
            .mount(&server)
            .await;

        let oauth = ClaudeOAuth::new().unwrap().with_token_url(format!("{}/token", server.uri()));
        let err = oauth
            .refresh_with_retry("rt-401", 3)
            .await
            .unwrap_err();
        assert_eq!(err.refresh_status(), Some(401));
    }
}
