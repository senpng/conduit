//! Anthropic Claude Code OAuth (authorization code + PKCE).
//!
//! Token exchange uses Firefox TLS fingerprint (CLIProxyAPI Anthropic auth /
//! utls Firefox on `api.anthropic.com`). Rust equivalent of
//! `HelloFirefox_Auto` = latest Firefox profile in current `wreq-util`.

use chrono::{Duration, Utc};
use serde::Deserialize;
use serde_json::json;
use wreq_util::{Emulation, Profile};

use crate::{credential::OAuthCredential, error::OAuthError, pkce::PkceCodes};

pub const AUTH_URL: &str = "https://claude.ai/oauth/authorize";
pub const TOKEN_URL: &str = "https://api.anthropic.com/v1/oauth/token";
pub const CLIENT_ID: &str = "9d1c250a-e61b-44d9-88ed-5944d1962f5e";
pub const REDIRECT_URI: &str = "http://localhost:54545/callback";
pub const CALLBACK_PORT: u16 = 54545;
pub const SCOPE: &str =
    "user:profile user:inference user:sessions:claude_code user:mcp_servers user:file_upload";

/// Align with Go utls `HelloFirefox_Auto`: latest Firefox preset in this wreq-util.
/// Bump when upgrading `wreq-util` if a newer `FirefoxN` is available.
#[inline]
fn firefox_auto() -> Profile {
    Profile::Firefox151
}

fn firefox_client() -> wreq::Client {
    // TLS/H2 fingerprint only — HTTP headers for token exchange are set explicitly.
    wreq::Client::builder()
        .emulation(
            Emulation::builder()
                .profile(firefox_auto())
                .headers(false)
                .http2(true)
                .build(),
        )
        .build()
        .expect("firefox oauth client")
}

pub struct ClaudeOAuth {
    client: wreq::Client,
    token_url: String,
    client_id: String,
    redirect_uri: String,
}

impl Default for ClaudeOAuth {
    fn default() -> Self {
        Self::new()
    }
}

impl ClaudeOAuth {
    pub fn new() -> Self {
        Self {
            client: firefox_client(),
            token_url: TOKEN_URL.into(),
            client_id: CLIENT_ID.into(),
            redirect_uri: REDIRECT_URI.into(),
        }
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

        let tr: ClaudeTokenResponse =
            serde_json::from_str(&text).map_err(|e| OAuthError::Serialization(e.to_string()))?;

        let expires_in = if tr.expires_in > 0 {
            tr.expires_in
        } else {
            3600
        };

        Ok(OAuthCredential {
            provider_type: "claude".into(),
            auth_kind: "oauth".into(),
            access_token: tr.access_token,
            refresh_token: tr.refresh_token.unwrap_or_default(),
            id_token: None,
            token_type: tr.token_type,
            expired: Some((Utc::now() + Duration::seconds(expires_in as i64)).to_rfc3339()),
            last_refresh: Some(Utc::now().to_rfc3339()),
            email: tr.account.and_then(|a| a.email_address),
            account_id: None,
            sub: None,
            base_url: Some(
                crate::credential::OAuthProviderKind::Claude
                    .default_base_url()
                    .into(),
            ),
            token_endpoint: Some(self.token_url.clone()),
            extra: Default::default(),
        })
    }

    pub async fn refresh(&self, refresh_token: &str) -> Result<OAuthCredential, OAuthError> {
        if refresh_token.is_empty() {
            return Err(OAuthError::Credential("refresh token is required".into()));
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
        let text = resp
            .text()
            .await
            .map_err(|e| OAuthError::Network(e.to_string()))?;
        if status != 200 {
            return Err(OAuthError::TokenRefresh { status, body: text });
        }

        let tr: ClaudeTokenResponse =
            serde_json::from_str(&text).map_err(|e| OAuthError::Serialization(e.to_string()))?;

        let expires_in = if tr.expires_in > 0 {
            tr.expires_in
        } else {
            3600
        };
        let new_refresh = tr
            .refresh_token
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| refresh_token.to_string());

        Ok(OAuthCredential {
            provider_type: "claude".into(),
            auth_kind: "oauth".into(),
            access_token: tr.access_token,
            refresh_token: new_refresh,
            id_token: None,
            token_type: tr.token_type,
            expired: Some((Utc::now() + Duration::seconds(expires_in as i64)).to_rfc3339()),
            last_refresh: Some(Utc::now().to_rfc3339()),
            email: tr.account.and_then(|a| a.email_address),
            account_id: None,
            sub: None,
            base_url: Some(
                crate::credential::OAuthProviderKind::Claude
                    .default_base_url()
                    .into(),
            ),
            token_endpoint: Some(self.token_url.clone()),
            extra: Default::default(),
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
    account: Option<ClaudeAccount>,
}

#[derive(Debug, Deserialize)]
struct ClaudeAccount {
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
        let oauth = ClaudeOAuth::new();
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

        let oauth = ClaudeOAuth::new().with_token_url(format!("{}/token", server.uri()));
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

        let oauth = ClaudeOAuth::new().with_token_url(format!("{}/token", server.uri()));
        let cred = oauth.refresh("rt-old").await.unwrap();
        assert_eq!(cred.access_token, "at-2");
        assert_eq!(cred.refresh_token, "rt-2");
    }
}
