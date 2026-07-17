//! OpenAI Codex / ChatGPT OAuth (authorization code + PKCE).

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use chrono::{Duration, Utc};
use serde::Deserialize;
use serde_json::Value;

use crate::{credential::OAuthCredential, error::OAuthError, pkce::PkceCodes};

pub const AUTH_URL: &str = "https://auth.openai.com/oauth/authorize";
pub const TOKEN_URL: &str = "https://auth.openai.com/oauth/token";
pub const CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
pub const REDIRECT_URI: &str = "http://localhost:1455/auth/callback";
pub const CALLBACK_PORT: u16 = 1455;
pub const SCOPE: &str = "openid email profile offline_access";

pub struct CodexOAuth {
    client: reqwest::Client,
    token_url: String,
    client_id: String,
    redirect_uri: String,
}

impl Default for CodexOAuth {
    fn default() -> Self {
        Self::new()
    }
}

impl CodexOAuth {
    pub fn new() -> Self {
        Self {
            client: reqwest::Client::new(),
            token_url: TOKEN_URL.into(),
            client_id: CLIENT_ID.into(),
            redirect_uri: REDIRECT_URI.into(),
        }
    }

    pub fn with_token_url(mut self, url: impl Into<String>) -> Self {
        self.token_url = url.into();
        self
    }

    pub fn generate_auth_url(&self, state: &str, pkce: &PkceCodes) -> String {
        let mut url = url::Url::parse(AUTH_URL).expect("static auth url");
        {
            let mut q = url.query_pairs_mut();
            q.append_pair("client_id", &self.client_id);
            q.append_pair("response_type", "code");
            q.append_pair("redirect_uri", &self.redirect_uri);
            q.append_pair("scope", SCOPE);
            q.append_pair("state", state);
            q.append_pair("code_challenge", &pkce.code_challenge);
            q.append_pair("code_challenge_method", "S256");
            q.append_pair("prompt", "login");
            q.append_pair("id_token_add_organizations", "true");
            q.append_pair("codex_cli_simplified_flow", "true");
        }
        url.to_string()
    }

    pub async fn exchange_code(
        &self,
        code: &str,
        pkce: &PkceCodes,
    ) -> Result<OAuthCredential, OAuthError> {
        self.exchange_code_with_redirect(code, &self.redirect_uri, pkce)
            .await
    }

    pub async fn exchange_code_with_redirect(
        &self,
        code: &str,
        redirect_uri: &str,
        pkce: &PkceCodes,
    ) -> Result<OAuthCredential, OAuthError> {
        let form = [
            ("grant_type", "authorization_code"),
            ("client_id", self.client_id.as_str()),
            ("code", code),
            ("redirect_uri", redirect_uri),
            ("code_verifier", pkce.code_verifier.as_str()),
        ];

        let resp = self
            .client
            .post(&self.token_url)
            .header("Content-Type", "application/x-www-form-urlencoded")
            .header("Accept", "application/json")
            .form(&form)
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

        self.parse_token_response(&text, None)
    }

    pub async fn refresh(&self, refresh_token: &str) -> Result<OAuthCredential, OAuthError> {
        if refresh_token.is_empty() {
            return Err(OAuthError::Credential("refresh token is required".into()));
        }
        let form = [
            ("client_id", self.client_id.as_str()),
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh_token),
            ("scope", "openid profile email"),
        ];

        let resp = self
            .client
            .post(&self.token_url)
            .header("Content-Type", "application/x-www-form-urlencoded")
            .header("Accept", "application/json")
            .form(&form)
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

        self.parse_token_response(&text, Some(refresh_token))
    }

    fn parse_token_response(
        &self,
        text: &str,
        fallback_refresh: Option<&str>,
    ) -> Result<OAuthCredential, OAuthError> {
        let tr: CodexTokenResponse =
            serde_json::from_str(text).map_err(|e| OAuthError::Serialization(e.to_string()))?;

        let (account_id, email) = tr
            .id_token
            .as_deref()
            .and_then(|t| parse_jwt_identity(t).ok())
            .unwrap_or((None, None));

        let expires_in = if tr.expires_in > 0 {
            tr.expires_in
        } else {
            3600
        };
        let refresh = tr
            .refresh_token
            .filter(|s| !s.is_empty())
            .or_else(|| fallback_refresh.map(|s| s.to_string()))
            .unwrap_or_default();

        Ok(OAuthCredential {
            provider_type: "codex".into(),
            auth_kind: "oauth".into(),
            access_token: tr.access_token,
            refresh_token: refresh,
            id_token: tr.id_token,
            token_type: tr.token_type,
            expired: Some((Utc::now() + Duration::seconds(expires_in as i64)).to_rfc3339()),
            last_refresh: Some(Utc::now().to_rfc3339()),
            email,
            account_id,
            sub: None,
            base_url: Some(
                crate::credential::OAuthProviderKind::Codex
                    .default_base_url()
                    .into(),
            ),
            token_endpoint: Some(self.token_url.clone()),
            extra: Default::default(),
        })
    }
}

#[derive(Debug, Deserialize)]
struct CodexTokenResponse {
    access_token: String,
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default)]
    id_token: Option<String>,
    #[serde(default)]
    token_type: Option<String>,
    #[serde(default)]
    expires_in: i64,
}

/// Decode JWT payload without signature verification (post-token-endpoint only).
pub fn parse_jwt_identity(token: &str) -> Result<(Option<String>, Option<String>), OAuthError> {
    let parts: Vec<&str> = token.split('.').collect();
    if parts.len() != 3 {
        return Err(OAuthError::Serialization("invalid JWT format".into()));
    }
    let payload = decode_jwt_segment(parts[1])?;
    let v: Value =
        serde_json::from_slice(&payload).map_err(|e| OAuthError::Serialization(e.to_string()))?;

    let email = v
        .get("email")
        .and_then(|e| e.as_str())
        .map(|s| s.to_string());

    let account_id = v
        .get("https://api.openai.com/auth")
        .and_then(|a| a.get("chatgpt_account_id"))
        .and_then(|id| id.as_str())
        .map(|s| s.to_string())
        .or_else(|| v.get("sub").and_then(|s| s.as_str()).map(|s| s.to_string()));

    Ok((account_id, email))
}

fn decode_jwt_segment(data: &str) -> Result<Vec<u8>, OAuthError> {
    let mut s = data.to_string();
    match s.len() % 4 {
        2 => s.push_str("=="),
        3 => s.push('='),
        _ => {}
    }
    URL_SAFE_NO_PAD
        .decode(s.trim_end_matches('=').as_bytes())
        .or_else(|_| {
            base64::engine::general_purpose::URL_SAFE.decode(
                match data.len() % 4 {
                    2 => format!("{data}=="),
                    3 => format!("{data}="),
                    _ => data.to_string(),
                }
                .as_bytes(),
            )
        })
        .map_err(|e| OAuthError::Serialization(format!("jwt base64: {e}")))
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use wiremock::{
        matchers::{method, path},
        Mock, MockServer, ResponseTemplate,
    };

    use super::*;
    use crate::pkce::generate_pkce;

    fn fake_id_token(account: &str, email: &str) -> String {
        let header = URL_SAFE_NO_PAD.encode(br#"{"alg":"none"}"#);
        let payload = URL_SAFE_NO_PAD.encode(
            json!({
                "email": email,
                "https://api.openai.com/auth": {
                    "chatgpt_account_id": account
                }
            })
            .to_string()
            .as_bytes(),
        );
        format!("{header}.{payload}.sig")
    }

    #[test]
    fn jwt_identity() {
        let t = fake_id_token("acct-1", "a@b.com");
        let (aid, email) = parse_jwt_identity(&t).unwrap();
        assert_eq!(aid.as_deref(), Some("acct-1"));
        assert_eq!(email.as_deref(), Some("a@b.com"));
    }

    #[tokio::test]
    async fn exchange_and_refresh() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "access_token": "codex-at",
                "refresh_token": "codex-rt",
                "id_token": fake_id_token("acc-9", "c@openai.com"),
                "expires_in": 3600
            })))
            .mount(&server)
            .await;

        let oauth = CodexOAuth::new().with_token_url(format!("{}/token", server.uri()));
        let pkce = generate_pkce().unwrap();
        let cred = oauth.exchange_code("code", &pkce).await.unwrap();
        assert_eq!(cred.access_token, "codex-at");
        assert_eq!(cred.account_id.as_deref(), Some("acc-9"));

        let refreshed = oauth.refresh("codex-rt").await.unwrap();
        assert_eq!(refreshed.access_token, "codex-at");
    }
}
