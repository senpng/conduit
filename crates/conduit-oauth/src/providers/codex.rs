//! OpenAI Codex / ChatGPT OAuth (authorization code + PKCE).

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use chrono::{Duration, Utc};
use serde::Deserialize;
use serde_json::Value;

use crate::{
    credential::OAuthCredential,
    error::OAuthError,
    pkce::PkceCodes,
    proxy::{apply_reqwest_proxy, env_proxy_url},
};

pub const AUTH_URL: &str = "https://auth.openai.com/oauth/authorize";
pub const TOKEN_URL: &str = "https://auth.openai.com/oauth/token";
pub const CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
pub const REDIRECT_URI: &str = "http://localhost:1455/auth/callback";
pub const CALLBACK_PORT: u16 = 1455;
pub const SCOPE: &str = "openid email profile offline_access";

/// Default max attempts for Codex refresh (CLIProxyAPI executor uses 3).
pub const DEFAULT_REFRESH_MAX_RETRIES: usize = 3;

fn build_client(proxy_url: Option<&str>) -> Result<reqwest::Client, OAuthError> {
    let builder = reqwest::Client::builder().timeout(std::time::Duration::from_secs(30));
    apply_reqwest_proxy(builder, proxy_url)?
        .build()
        .map_err(|e| OAuthError::Network(format!("codex oauth client: {e}")))
}

pub struct CodexOAuth {
    client: reqwest::Client,
    token_url: String,
    client_id: String,
    redirect_uri: String,
}

impl CodexOAuth {
    /// Build with proxy from env. Configured proxy failures do **not** fall back to direct.
    pub fn new() -> Result<Self, OAuthError> {
        Self::with_proxy_url(env_proxy_url())
    }

    pub fn with_proxy_url(proxy_url: Option<String>) -> Result<Self, OAuthError> {
        Ok(Self {
            client: build_client(proxy_url.as_deref())?,
            token_url: TOKEN_URL.into(),
            client_id: CLIENT_ID.into(),
            redirect_uri: REDIRECT_URI.into(),
        })
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

    /// Refresh with linear backoff (CLIProxyAPI `RefreshTokensWithRetry`).
    ///
    /// Retries most errors; only `refresh_token_reused` is treated as fatal
    /// (matches CLIProxyAPI `isNonRetryableRefreshErr`).
    pub async fn refresh_with_retry(
        &self,
        refresh_token: &str,
        max_retries: usize,
    ) -> Result<OAuthCredential, OAuthError> {
        let max_retries = max_retries.max(1);
        let mut last_err = OAuthError::Credential("token refresh failed".into());
        for attempt in 0..max_retries {
            if attempt > 0 {
                tokio::time::sleep(std::time::Duration::from_secs(attempt as u64)).await;
            }
            match self.refresh(refresh_token).await {
                Ok(cred) => return Ok(cred),
                Err(e) => {
                    last_err = e;
                    if is_non_retryable_codex_refresh(&last_err) {
                        return Err(last_err);
                    }
                }
            }
        }
        Err(last_err)
    }

    fn parse_token_response(
        &self,
        text: &str,
        fallback_refresh: Option<&str>,
    ) -> Result<OAuthCredential, OAuthError> {
        let tr: CodexTokenResponse =
            serde_json::from_str(text).map_err(|e| OAuthError::Serialization(e.to_string()))?;

        let identity = tr
            .id_token
            .as_deref()
            .and_then(|t| parse_jwt_identity(t).ok())
            .unwrap_or_default();

        // CLIProxyAPI uses expires_in as-is (Go zero → immediate expiry).
        let expires_in = tr.expires_in.max(0);
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
            expired: Some((Utc::now() + Duration::seconds(expires_in)).to_rfc3339()),
            last_refresh: Some(Utc::now().to_rfc3339()),
            email: identity.email.clone(),
            account_id: identity.account_id.clone(),
            plan_type: identity.plan_type.clone(),
            organization_id: None,
            organization_name: None,
            sub: None,
            base_url: Some(
                crate::credential::OAuthProviderKind::Codex
                    .default_base_url()
                    .into(),
            ),
            token_endpoint: Some(self.token_url.clone()),
            proxy_url: None,
            using_api: None,
            extra: identity_to_extra(&identity),
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

fn is_non_retryable_codex_refresh(err: &OAuthError) -> bool {
    // CLIProxyAPI only short-circuits on "refresh_token_reused".
    err.to_string()
        .to_ascii_lowercase()
        .contains("refresh_token_reused")
}

/// Claims extracted from Codex id_token (no signature verification).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CodexJwtIdentity {
    pub account_id: Option<String>,
    pub email: Option<String>,
    pub plan_type: Option<String>,
    pub chatgpt_user_id: Option<String>,
    pub user_id: Option<String>,
    pub auth_provider: Option<String>,
    pub organizations: Option<Value>,
    pub subscription_active_start: Option<Value>,
    pub subscription_active_until: Option<Value>,
}

/// Decode JWT payload without signature verification (post-token-endpoint only).
pub fn parse_jwt_identity(token: &str) -> Result<CodexJwtIdentity, OAuthError> {
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

    let auth = v.get("https://api.openai.com/auth");
    let account_id = auth
        .and_then(|a| a.get("chatgpt_account_id"))
        .and_then(|id| id.as_str())
        .map(|s| s.to_string())
        .or_else(|| v.get("sub").and_then(|s| s.as_str()).map(|s| s.to_string()));

    let plan_type = auth
        .and_then(|a| a.get("chatgpt_plan_type"))
        .and_then(|p| p.as_str())
        .map(|s| s.to_string())
        .filter(|s| !s.trim().is_empty());

    let chatgpt_user_id = auth
        .and_then(|a| a.get("chatgpt_user_id"))
        .and_then(|p| p.as_str())
        .map(|s| s.to_string());
    let user_id = auth
        .and_then(|a| a.get("user_id"))
        .and_then(|p| p.as_str())
        .map(|s| s.to_string());
    let organizations = auth.and_then(|a| a.get("organizations")).cloned();
    let subscription_active_start = auth
        .and_then(|a| a.get("chatgpt_subscription_active_start"))
        .cloned();
    let subscription_active_until = auth
        .and_then(|a| a.get("chatgpt_subscription_active_until"))
        .cloned();
    let auth_provider = v
        .get("auth_provider")
        .and_then(|p| p.as_str())
        .map(|s| s.to_string());

    Ok(CodexJwtIdentity {
        account_id,
        email,
        plan_type,
        chatgpt_user_id,
        user_id,
        auth_provider,
        organizations,
        subscription_active_start,
        subscription_active_until,
    })
}

/// Flatten rich JWT claims into credential `extra` for persistence / console.
pub fn identity_to_extra(id: &CodexJwtIdentity) -> std::collections::HashMap<String, Value> {
    let mut m = std::collections::HashMap::new();
    if let Some(ref v) = id.chatgpt_user_id {
        m.insert("chatgpt_user_id".into(), Value::String(v.clone()));
    }
    if let Some(ref v) = id.user_id {
        m.insert("user_id".into(), Value::String(v.clone()));
    }
    if let Some(ref v) = id.auth_provider {
        m.insert("auth_provider".into(), Value::String(v.clone()));
    }
    if let Some(ref v) = id.organizations {
        m.insert("organizations".into(), v.clone());
    }
    if let Some(ref v) = id.subscription_active_start {
        m.insert("chatgpt_subscription_active_start".into(), v.clone());
    }
    if let Some(ref v) = id.subscription_active_until {
        m.insert("chatgpt_subscription_active_until".into(), v.clone());
    }
    if let Some(ref v) = id.plan_type {
        m.insert("plan_type".into(), Value::String(v.clone()));
    }
    m
}

/// Normalize plan type for ids (CLIProxyAPI `normalizePlanTypeForFilename`).
pub fn normalize_plan_type(plan_type: &str) -> String {
    let parts: Vec<String> = plan_type
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|p| !p.is_empty())
        .map(|p| p.to_ascii_lowercase())
        .collect();
    parts.join("-")
}

fn is_team_scoped_plan(plan: &str) -> bool {
    plan == "team" || plan == "k12"
}

/// 8-char hex of SHA-256(account_id) for team-scoped filenames (CLIProxyAPI).
pub fn hash_account_id(account_id: &str) -> String {
    use sha2::{Digest, Sha256};
    let dig = Sha256::digest(account_id.as_bytes());
    hex::encode(&dig[..4]) // 8 hex chars
}

fn sanitize_id_part(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' {
                c
            } else if c == '@' {
                '_'
            } else {
                '-'
            }
        })
        .collect::<String>()
        .trim_matches('-')
        .to_string()
}

/// Stable provider id stem (CLIProxyAPI `CredentialFileName` without `.json`).
///
/// - no plan: `codex-{email}`
/// - plan (non-team): `codex-{email}-{plan}`
/// - team/k12 + account: `codex-{accountHash}-{email}-{plan}`
pub fn stable_provider_id(
    email: Option<&str>,
    plan_type: Option<&str>,
    account_id: Option<&str>,
) -> Option<String> {
    let email = email.map(str::trim).filter(|s| !s.is_empty())?;
    let email_part = sanitize_id_part(email);
    if email_part.is_empty() {
        return None;
    }
    let plan = plan_type
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(normalize_plan_type)
        .filter(|s| !s.is_empty());

    Some(match plan {
        None => format!("codex-{email_part}"),
        Some(plan) if is_team_scoped_plan(&plan) => {
            if let Some(aid) = account_id.map(str::trim).filter(|s| !s.is_empty()) {
                let h = hash_account_id(aid);
                format!("codex-{h}-{email_part}-{plan}")
            } else {
                format!("codex-{email_part}-{plan}")
            }
        }
        Some(plan) => format!("codex-{email_part}-{plan}"),
    })
}

/// Human-readable provider display name.
pub fn display_provider_name(
    email: Option<&str>,
    plan_type: Option<&str>,
) -> String {
    let email = email.unwrap_or("").trim();
    let plan = plan_type
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(normalize_plan_type);
    match (email.is_empty(), plan) {
        (true, Some(p)) => format!("codex ({p})"),
        (true, None) => "codex-oauth".into(),
        (false, Some(p)) => format!("codex ({email}, {p})"),
        (false, None) => format!("codex ({email})"),
    }
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

    fn fake_id_token(account: &str, email: &str, plan: Option<&str>) -> String {
        let header = URL_SAFE_NO_PAD.encode(br#"{"alg":"none"}"#);
        let mut auth = json!({
            "chatgpt_account_id": account
        });
        if let Some(p) = plan {
            auth["chatgpt_plan_type"] = json!(p);
        }
        let payload = URL_SAFE_NO_PAD.encode(
            json!({
                "email": email,
                "https://api.openai.com/auth": auth
            })
            .to_string()
            .as_bytes(),
        );
        format!("{header}.{payload}.sig")
    }

    #[test]
    fn jwt_identity() {
        let t = fake_id_token("acct-1", "a@b.com", Some("plus"));
        let id = parse_jwt_identity(&t).unwrap();
        assert_eq!(id.account_id.as_deref(), Some("acct-1"));
        assert_eq!(id.email.as_deref(), Some("a@b.com"));
        assert_eq!(id.plan_type.as_deref(), Some("plus"));
    }

    #[test]
    fn stable_ids_match_cliproxy_patterns() {
        assert_eq!(
            stable_provider_id(Some("u@x.com"), None, Some("acc")).as_deref(),
            Some("codex-u_x.com")
        );
        assert_eq!(
            stable_provider_id(Some("u@x.com"), Some("plus"), Some("acc")).as_deref(),
            Some("codex-u_x.com-plus")
        );
        let team = stable_provider_id(Some("u@x.com"), Some("team"), Some("acc-team-1")).unwrap();
        assert!(team.starts_with("codex-"));
        assert!(team.ends_with("-u_x.com-team"));
        assert_eq!(team.split('-').count() >= 4, true);
    }

    #[tokio::test]
    async fn exchange_and_refresh() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "access_token": "codex-at",
                "refresh_token": "codex-rt",
                "id_token": fake_id_token("acc-9", "c@openai.com", Some("pro")),
                "expires_in": 3600
            })))
            .mount(&server)
            .await;

        let oauth = CodexOAuth::new().unwrap().with_token_url(format!("{}/token", server.uri()));
        let pkce = generate_pkce().unwrap();
        let cred = oauth.exchange_code("code", &pkce).await.unwrap();
        assert_eq!(cred.access_token, "codex-at");
        assert_eq!(cred.account_id.as_deref(), Some("acc-9"));
        assert_eq!(cred.plan_type.as_deref(), Some("pro"));

        let refreshed = oauth.refresh("codex-rt").await.unwrap();
        assert_eq!(refreshed.access_token, "codex-at");
    }
}
