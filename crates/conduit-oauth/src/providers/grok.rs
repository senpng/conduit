//! xAI Grok CLI OAuth — Device Authorization Grant (RFC 8628).

use chrono::{Duration, Utc};
use serde::Deserialize;
use serde_json::Value;

use crate::{
    credential::OAuthCredential,
    error::OAuthError,
    proxy::{apply_reqwest_proxy, env_proxy_url},
};

pub const ISSUER: &str = "https://auth.x.ai";
pub const DISCOVERY_URL: &str = "https://auth.x.ai/.well-known/openid-configuration";
pub const CLIENT_ID: &str = "b1a00492-073a-47ea-816f-4c329264a828";
pub const SCOPE: &str = "openid profile email offline_access grok-cli:access api:access";
pub const DEVICE_CODE_GRANT: &str = "urn:ietf:params:oauth:grant-type:device_code";
/// Official xAI API base — stored on OAuth credentials (CLIProxyAPI `DefaultAPIBaseURL`).
pub const DEFAULT_API_BASE: &str = "https://api.x.ai/v1";
/// Grok CLI chat-proxy — OAuth subscription **chat** rewrites empty/official base here.
pub const CLI_CHAT_PROXY_BASE: &str = "https://cli-chat-proxy.grok.com/v1";
/// Client version expected by cli-chat-proxy (keep in sync with Grok CLI).
pub const CLI_CLIENT_VERSION: &str = "0.2.93";
pub const DEFAULT_POLL_INTERVAL_SECS: u64 = 5;
pub const MAX_POLL_DURATION_SECS: u64 = 30 * 60;

/// Resolve chat base URL for Grok OAuth (CLIProxyAPI `xaiChatBaseURL` semantics).
///
/// - `using_api == true`: official API path — empty → `DEFAULT_API_BASE`, else honor configured.
/// - `using_api == false` (OAuth subscription default): empty or official default →
///   rewrite to cli-chat-proxy; explicit custom base is honored.
pub fn resolve_oauth_chat_base(configured: Option<&str>, using_api: bool) -> String {
    let b = configured.unwrap_or("").trim().trim_end_matches('/');
    if using_api {
        if b.is_empty() {
            return DEFAULT_API_BASE.to_string();
        }
        return b.to_string();
    }
    if b.is_empty() || is_official_api_base(b) {
        return CLI_CHAT_PROXY_BASE.to_string();
    }
    b.to_string()
}

pub fn is_official_api_base(base: &str) -> bool {
    let n = base.trim().trim_end_matches('/');
    n == DEFAULT_API_BASE || n == "https://api.x.ai"
}

pub fn is_cli_chat_proxy_base(base: &str) -> bool {
    let n = base.trim().trim_end_matches('/');
    n == CLI_CHAT_PROXY_BASE || n == "https://cli-chat-proxy.grok.com"
}

/// Identity headers required by cli-chat-proxy for OAuth/CLI clients.
pub fn cli_proxy_headers() -> Vec<(String, String)> {
    vec![
        ("X-XAI-Token-Auth".into(), "xai-grok-cli".into()),
        ("x-grok-client-version".into(), CLI_CLIENT_VERSION.into()),
        (
            "User-Agent".into(),
            format!("xai-grok-workspace/{CLI_CLIENT_VERSION}"),
        ),
    ]
}

#[derive(Debug, Clone)]
pub struct Discovery {
    pub device_authorization_endpoint: String,
    pub token_endpoint: String,
}

#[derive(Debug, Clone)]
pub struct DeviceCodeResponse {
    pub device_code: String,
    pub user_code: String,
    pub verification_uri: String,
    pub verification_uri_complete: Option<String>,
    pub expires_in: u64,
    pub interval: u64,
    pub token_endpoint: String,
}

fn build_client(proxy_url: Option<&str>) -> Result<reqwest::Client, OAuthError> {
    let builder = reqwest::Client::builder().timeout(std::time::Duration::from_secs(30));
    apply_reqwest_proxy(builder, proxy_url)?
        .build()
        .map_err(|e| OAuthError::Network(format!("grok oauth client: {e}")))
}

pub struct GrokOAuth {
    client: reqwest::Client,
    discovery_url: String,
    client_id: String,
}

impl GrokOAuth {
    /// Build with proxy from env. Configured proxy failures do **not** fall back to direct.
    pub fn new() -> Result<Self, OAuthError> {
        Self::with_proxy_url(env_proxy_url())
    }

    pub fn with_proxy_url(proxy_url: Option<String>) -> Result<Self, OAuthError> {
        Ok(Self {
            client: build_client(proxy_url.as_deref())?,
            discovery_url: DISCOVERY_URL.into(),
            client_id: CLIENT_ID.into(),
        })
    }

    pub fn with_discovery_url(mut self, url: impl Into<String>) -> Self {
        self.discovery_url = url.into();
        self
    }

    pub async fn discover(&self) -> Result<Discovery, OAuthError> {
        let resp = self
            .client
            .get(&self.discovery_url)
            .header("Accept", "application/json")
            .send()
            .await
            .map_err(|e| OAuthError::Network(e.to_string()))?;
        let status = resp.status().as_u16();
        let text = resp
            .text()
            .await
            .map_err(|e| OAuthError::Network(e.to_string()))?;
        if status != 200 {
            return Err(OAuthError::Provider(format!(
                "xai discovery failed ({status}): {text}"
            )));
        }
        let v: Value =
            serde_json::from_str(&text).map_err(|e| OAuthError::Serialization(e.to_string()))?;
        let device = v
            .get("device_authorization_endpoint")
            .and_then(|x| x.as_str())
            .ok_or_else(|| OAuthError::Provider("missing device_authorization_endpoint".into()))?;
        let token = v
            .get("token_endpoint")
            .and_then(|x| x.as_str())
            .ok_or_else(|| OAuthError::Provider("missing token_endpoint".into()))?;

        Ok(Discovery {
            device_authorization_endpoint: validate_xai_endpoint(
                device,
                "device_authorization_endpoint",
            )?,
            token_endpoint: validate_xai_endpoint(token, "token_endpoint")?,
        })
    }

    /// Discover endpoints then request a device code.
    pub async fn start_device_flow(&self) -> Result<DeviceCodeResponse, OAuthError> {
        let d = self.discover().await?;
        self.request_device_code(&d.device_authorization_endpoint, &d.token_endpoint)
            .await
    }

    pub async fn request_device_code(
        &self,
        device_authorization_endpoint: &str,
        token_endpoint: &str,
    ) -> Result<DeviceCodeResponse, OAuthError> {
        let form = [("client_id", self.client_id.as_str()), ("scope", SCOPE)];
        let resp = self
            .client
            .post(device_authorization_endpoint)
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
            return Err(OAuthError::Provider(format!(
                "device code request failed ({status}): {text}"
            )));
        }
        let raw: DeviceCodeRaw =
            serde_json::from_str(&text).map_err(|e| OAuthError::Serialization(e.to_string()))?;
        if raw.device_code.is_empty() || raw.user_code.is_empty() {
            return Err(OAuthError::Provider(
                "device code response incomplete".into(),
            ));
        }
        let verification_uri = raw
            .verification_uri
            .or(raw.verification_uri_complete.clone())
            .filter(|s| !s.is_empty())
            .ok_or_else(|| OAuthError::Provider("missing verification_uri".into()))?;

        Ok(DeviceCodeResponse {
            device_code: raw.device_code,
            user_code: raw.user_code,
            verification_uri,
            verification_uri_complete: raw.verification_uri_complete,
            expires_in: if raw.expires_in > 0 {
                raw.expires_in
            } else {
                900
            },
            interval: if raw.interval > 0 {
                raw.interval
            } else {
                DEFAULT_POLL_INTERVAL_SECS
            },
            token_endpoint: token_endpoint.to_string(),
        })
    }

    /// Single poll attempt. Returns `Ok(None)` when still pending.
    pub async fn poll_once(
        &self,
        token_endpoint: &str,
        device_code: &str,
    ) -> Result<Option<OAuthCredential>, OAuthError> {
        let form = [
            ("grant_type", DEVICE_CODE_GRANT),
            ("device_code", device_code),
            ("client_id", self.client_id.as_str()),
        ];
        let resp = self
            .client
            .post(token_endpoint)
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

        let payload: DeviceTokenRaw = serde_json::from_str(&text).unwrap_or(DeviceTokenRaw {
            error: None,
            error_description: None,
            access_token: None,
            refresh_token: None,
            id_token: None,
            token_type: None,
            expires_in: 0,
        });

        if let Some(ref err) = payload.error {
            return match err.as_str() {
                "authorization_pending" => Ok(None),
                "slow_down" => Err(OAuthError::AuthorizationPending), // caller should back off
                "expired_token" => Err(OAuthError::DeviceCodeExpired),
                "access_denied" => Err(OAuthError::AccessDenied),
                other => Err(OAuthError::Provider(format!(
                    "xai device token error: {other}: {}",
                    payload.error_description.unwrap_or_default()
                ))),
            };
        }

        if status != 200 {
            // Some servers return pending as HTTP 400 with error field already handled
            return Err(OAuthError::TokenExchange { status, body: text });
        }

        let access = payload
            .access_token
            .filter(|s| !s.is_empty())
            .ok_or_else(|| OAuthError::Provider("missing access_token".into()))?;

        let (email, sub) = payload
            .id_token
            .as_deref()
            .and_then(|t| parse_id_token_identity(t).ok())
            .unwrap_or((None, None));

        // CLIProxyAPI uses expires_in as-is (Go zero → immediate expiry).
        let expires_in = payload.expires_in.max(0);

        Ok(Some(OAuthCredential {
            provider_type: "xai".into(),
            auth_kind: "oauth".into(),
            access_token: access,
            refresh_token: payload.refresh_token.unwrap_or_default(),
            id_token: payload.id_token,
            token_type: payload.token_type,
            expired: Some((Utc::now() + Duration::seconds(expires_in)).to_rfc3339()),
            last_refresh: Some(Utc::now().to_rfc3339()),
            email,
            account_id: None,
            plan_type: None,
            organization_id: None,
            organization_name: None,
            sub,
            // Persist official API base (CLIProxyAPI AuthBundle.BaseURL = DefaultAPIBaseURL).
            // Chat rewrites to cli-chat-proxy at request time via resolve_oauth_chat_base.
            base_url: Some(DEFAULT_API_BASE.into()),
            token_endpoint: Some(token_endpoint.to_string()),
            proxy_url: None,
            // OAuth subscription default: chat-proxy (using_api=false).
            using_api: Some(false),
            cloak_mode: None,
            extra: Default::default(),
        }))
    }

    /// Poll until authorized, expired, or cancelled.
    ///
    /// `cancel` is checked each loop iteration (session cancel / explicit abort).
    pub async fn wait_for_authorization(
        &self,
        device: &DeviceCodeResponse,
    ) -> Result<OAuthCredential, OAuthError> {
        self.wait_for_authorization_cancellable(device, || false)
            .await
    }

    /// Like [`wait_for_authorization`] with a cancel predicate.
    ///
    /// `is_cancelled` returns true when the login should abort (e.g. session cancelled).
    pub async fn wait_for_authorization_cancellable(
        &self,
        device: &DeviceCodeResponse,
        mut is_cancelled: impl FnMut() -> bool,
    ) -> Result<OAuthCredential, OAuthError> {
        let mut interval =
            std::time::Duration::from_secs(device.interval.max(DEFAULT_POLL_INTERVAL_SECS));
        let deadline = std::time::Instant::now()
            + std::time::Duration::from_secs(device.expires_in.clamp(1, MAX_POLL_DURATION_SECS));

        loop {
            if is_cancelled() {
                return Err(OAuthError::SessionCancelled);
            }
            match self
                .poll_once(&device.token_endpoint, &device.device_code)
                .await
            {
                Ok(Some(cred)) => return Ok(cred),
                Ok(None) => {}
                Err(OAuthError::AuthorizationPending) => {
                    // slow_down
                    interval += std::time::Duration::from_secs(DEFAULT_POLL_INTERVAL_SECS);
                }
                Err(e) => return Err(e),
            }
            if std::time::Instant::now() >= deadline {
                return Err(OAuthError::DeviceCodeExpired);
            }
            // Interruptible sleep: wake early to re-check cancel.
            let slice = interval.min(std::time::Duration::from_secs(1));
            let mut slept = std::time::Duration::ZERO;
            while slept < interval {
                if is_cancelled() {
                    return Err(OAuthError::SessionCancelled);
                }
                tokio::time::sleep(slice).await;
                slept += slice;
            }
        }
    }

    pub async fn refresh(
        &self,
        refresh_token: &str,
        token_endpoint: Option<&str>,
    ) -> Result<OAuthCredential, OAuthError> {
        if refresh_token.is_empty() {
            return Err(OAuthError::Credential("refresh token is required".into()));
        }
        let endpoint = if let Some(e) = token_endpoint.filter(|s| !s.is_empty()) {
            e.to_string()
        } else {
            self.discover().await?.token_endpoint
        };

        let form = [
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh_token),
            ("client_id", self.client_id.as_str()),
        ];
        let resp = self
            .client
            .post(&endpoint)
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
        let payload: DeviceTokenRaw =
            serde_json::from_str(&text).map_err(|e| OAuthError::Serialization(e.to_string()))?;
        let access = payload
            .access_token
            .filter(|s| !s.is_empty())
            .ok_or_else(|| OAuthError::Provider("refresh missing access_token".into()))?;
        let new_refresh = payload
            .refresh_token
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| refresh_token.to_string());
        let expires_in = payload.expires_in.max(0);
        let (email, sub) = payload
            .id_token
            .as_deref()
            .and_then(|t| parse_id_token_identity(t).ok())
            .unwrap_or((None, None));

        Ok(OAuthCredential {
            provider_type: "xai".into(),
            auth_kind: "oauth".into(),
            access_token: access,
            refresh_token: new_refresh,
            id_token: payload.id_token,
            token_type: payload.token_type,
            expired: Some((Utc::now() + Duration::seconds(expires_in)).to_rfc3339()),
            last_refresh: Some(Utc::now().to_rfc3339()),
            email,
            account_id: None,
            plan_type: None,
            organization_id: None,
            organization_name: None,
            sub,
            base_url: Some(DEFAULT_API_BASE.into()),
            token_endpoint: Some(endpoint),
            proxy_url: None,
            using_api: Some(false),
            cloak_mode: None,
            extra: Default::default(),
        })
    }
}

/// Validate discovery endpoint is https and on x.ai.
pub fn validate_xai_endpoint(raw: &str, field: &str) -> Result<String, OAuthError> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Err(OAuthError::Provider(format!(
            "xai discovery {field} is empty"
        )));
    }
    let parsed = url::Url::parse(raw)
        .map_err(|e| OAuthError::Provider(format!("xai discovery {field} invalid: {e}")))?;
    if parsed.scheme() != "https" {
        return Err(OAuthError::Provider(format!(
            "xai discovery {field} must use https"
        )));
    }
    let host = parsed.host_str().unwrap_or("").to_ascii_lowercase();
    if host != "x.ai" && !host.ends_with(".x.ai") {
        // Allow localhost/wiremock for tests
        if host != "127.0.0.1" && host != "localhost" {
            return Err(OAuthError::Provider(format!(
                "xai discovery {field} host {host:?} is not on x.ai"
            )));
        }
    }
    Ok(raw.to_string())
}

fn parse_id_token_identity(token: &str) -> Result<(Option<String>, Option<String>), OAuthError> {
    use base64::Engine;
    let parts: Vec<&str> = token.split('.').collect();
    if parts.len() != 3 {
        return Ok((None, None));
    }
    let mut s = parts[1].to_string();
    match s.len() % 4 {
        2 => s.push_str("=="),
        3 => s.push('='),
        _ => {}
    }
    let bytes = base64::engine::general_purpose::URL_SAFE
        .decode(s.as_bytes())
        .or_else(|_| base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(parts[1].as_bytes()))
        .map_err(|e| OAuthError::Serialization(e.to_string()))?;
    let v: Value =
        serde_json::from_slice(&bytes).map_err(|e| OAuthError::Serialization(e.to_string()))?;
    let email = v.get("email").and_then(|e| e.as_str()).map(str::to_string);
    let sub = v.get("sub").and_then(|e| e.as_str()).map(str::to_string);
    Ok((email, sub))
}

#[derive(Debug, Deserialize)]
struct DeviceCodeRaw {
    device_code: String,
    user_code: String,
    #[serde(default)]
    verification_uri: Option<String>,
    #[serde(default)]
    verification_uri_complete: Option<String>,
    #[serde(default)]
    expires_in: u64,
    #[serde(default)]
    interval: u64,
}

#[derive(Debug, Deserialize)]
struct DeviceTokenRaw {
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    error_description: Option<String>,
    #[serde(default)]
    access_token: Option<String>,
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default)]
    id_token: Option<String>,
    #[serde(default)]
    token_type: Option<String>,
    #[serde(default)]
    expires_in: i64,
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use wiremock::{
        matchers::{method, path},
        Mock, MockServer, ResponseTemplate,
    };

    use super::*;

    #[test]
    fn validate_endpoint_accepts_xai() {
        assert!(validate_xai_endpoint("https://auth.x.ai/oauth/token", "token").is_ok());
        assert!(validate_xai_endpoint("http://evil.com/t", "token").is_err());
    }

    #[test]
    fn oauth_chat_base_rewrites_official_api_to_cli_proxy() {
        assert_eq!(resolve_oauth_chat_base(None, false), CLI_CHAT_PROXY_BASE);
        assert_eq!(
            resolve_oauth_chat_base(Some("https://api.x.ai/v1"), false),
            CLI_CHAT_PROXY_BASE
        );
        assert_eq!(
            resolve_oauth_chat_base(Some("https://api.x.ai/v1/"), false),
            CLI_CHAT_PROXY_BASE
        );
        assert_eq!(
            resolve_oauth_chat_base(Some("https://custom.example/v1"), false),
            "https://custom.example/v1"
        );
        assert_eq!(
            resolve_oauth_chat_base(Some(CLI_CHAT_PROXY_BASE), false),
            CLI_CHAT_PROXY_BASE
        );
    }

    #[test]
    fn oauth_chat_base_using_api_keeps_official() {
        assert_eq!(resolve_oauth_chat_base(None, true), DEFAULT_API_BASE);
        assert_eq!(
            resolve_oauth_chat_base(Some("https://api.x.ai/v1"), true),
            "https://api.x.ai/v1"
        );
        assert_eq!(
            resolve_oauth_chat_base(Some("https://custom.example/v1"), true),
            "https://custom.example/v1"
        );
    }

    #[tokio::test]
    async fn device_flow_mock() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/.well-known/openid-configuration"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "device_authorization_endpoint": format!("{}/device", server.uri()),
                "token_endpoint": format!("{}/token", server.uri())
            })))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/device"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "device_code": "dc",
                "user_code": "ABCD-EFGH",
                "verification_uri": "https://auth.x.ai/device",
                "expires_in": 600,
                "interval": 1
            })))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "access_token": "grok-at",
                "refresh_token": "grok-rt",
                "expires_in": 3600
            })))
            .mount(&server)
            .await;

        // Bypass host validation by calling request_device_code directly with mock URLs
        let oauth = GrokOAuth::new().unwrap();
        let device = oauth
            .request_device_code(
                &format!("{}/device", server.uri()),
                &format!("{}/token", server.uri()),
            )
            .await
            .unwrap();
        assert_eq!(device.user_code, "ABCD-EFGH");
        let cred = oauth
            .poll_once(&device.token_endpoint, &device.device_code)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(cred.access_token, "grok-at");
    }
}
