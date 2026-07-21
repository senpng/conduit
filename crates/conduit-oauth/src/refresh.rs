//! Token refresh with per-refresh-token singleflight.

use std::{collections::HashMap, sync::Arc};

use chrono::Duration;
use parking_lot::Mutex;
use tokio::sync::Mutex as AsyncMutex;

use crate::{
    credential::{OAuthCredential, OAuthProviderKind},
    error::OAuthError,
    providers::{ClaudeOAuth, CodexOAuth, GrokOAuth},
    proxy::resolve_effective_proxy,
};

/// Default lead time before expiry to refresh proactively.
pub fn default_refresh_lead() -> Duration {
    Duration::minutes(5)
}

type InflightMap = HashMap<String, Arc<AsyncMutex<Option<Result<OAuthCredential, String>>>>>;

/// Coordinates concurrent refreshes so the same refresh_token is only exchanged once.
#[derive(Clone, Default)]
pub struct RefreshCoordinator {
    inflight: Arc<Mutex<InflightMap>>,
    lead: Duration,
    /// Daemon-level proxy URL (config). Credential `proxy_url` and env still win.
    default_proxy: Option<String>,
}

impl RefreshCoordinator {
    pub fn new() -> Self {
        Self {
            inflight: Arc::new(Mutex::new(HashMap::new())),
            lead: default_refresh_lead(),
            default_proxy: None,
        }
    }

    pub fn with_lead(mut self, lead: Duration) -> Self {
        self.lead = lead;
        self
    }

    /// Set daemon config proxy (CLIProxyAPI `cfg.ProxyURL`).
    pub fn with_default_proxy(mut self, proxy: Option<String>) -> Self {
        self.default_proxy = proxy
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());
        self
    }

    pub fn lead(&self) -> Duration {
        self.lead
    }

    fn proxy_for(&self, cred: &OAuthCredential) -> Option<String> {
        resolve_effective_proxy(cred.proxy_url_override(), self.default_proxy.as_deref())
    }

    /// Refresh if needed; returns the (possibly updated) credential.
    pub async fn ensure_fresh(&self, cred: OAuthCredential) -> Result<OAuthCredential, OAuthError> {
        if !cred.needs_refresh(self.lead) {
            return Ok(cred);
        }
        if cred.refresh_token.is_empty() {
            return Err(OAuthError::Credential(
                "access token expired and no refresh_token available".into(),
            ));
        }
        self.refresh(cred).await
    }

    pub async fn refresh(&self, cred: OAuthCredential) -> Result<OAuthCredential, OAuthError> {
        let key = cred.refresh_token.clone();
        let slot = {
            let mut map = self.inflight.lock();
            map.entry(key.clone())
                .or_insert_with(|| Arc::new(AsyncMutex::new(None)))
                .clone()
        };

        let mut guard = slot.lock().await;
        if let Some(cached) = guard.as_ref() {
            return match cached {
                Ok(c) => Ok(c.clone()),
                Err(e) => Err(OAuthError::TokenRefresh {
                    status: 0,
                    body: e.clone(),
                }),
            };
        }

        let result = self.do_refresh(&cred).await;
        *guard = Some(match &result {
            Ok(c) => Ok(c.clone()),
            Err(e) => Err(e.to_string()),
        });
        // Drop inflight entry so a later refresh can run again
        drop(guard);
        self.inflight.lock().remove(&key);
        result
    }

    async fn do_refresh(&self, cred: &OAuthCredential) -> Result<OAuthCredential, OAuthError> {
        let kind = cred.kind()?;
        let proxy = self.proxy_for(cred);
        let mut fresh = match kind {
            OAuthProviderKind::Claude => {
                ClaudeOAuth::with_proxy_url(proxy)?
                    .refresh_with_retry(
                        &cred.refresh_token,
                        crate::providers::claude::DEFAULT_REFRESH_MAX_RETRIES,
                    )
                    .await?
            }
            OAuthProviderKind::Codex => {
                CodexOAuth::with_proxy_url(proxy)?
                    .refresh_with_retry(
                        &cred.refresh_token,
                        crate::providers::codex::DEFAULT_REFRESH_MAX_RETRIES,
                    )
                    .await?
            }
            OAuthProviderKind::Xai => {
                GrokOAuth::with_proxy_url(proxy)?
                    .refresh(&cred.refresh_token, cred.token_endpoint.as_deref())
                    .await?
            }
        };
        // Prefer previously stored metadata: refresh endpoints often omit email /
        // account / custom base_url (CLIProxyAPI keeps file fields across refresh).
        if let Some(ref b) = cred.base_url {
            if !b.is_empty() {
                fresh.base_url = Some(b.clone());
            }
        }
        if fresh.email.is_none() {
            fresh.email = cred.email.clone();
        }
        if fresh.account_id.is_none() {
            fresh.account_id = cred.account_id.clone();
        }
        if fresh.plan_type.is_none() {
            fresh.plan_type = cred.plan_type.clone();
        }
        if fresh.organization_id.is_none() {
            fresh.organization_id = cred.organization_id.clone();
        }
        if fresh.organization_name.is_none() {
            fresh.organization_name = cred.organization_name.clone();
        }
        if let Some(ref te) = cred.token_endpoint {
            if !te.is_empty() {
                fresh.token_endpoint = Some(te.clone());
            }
        }
        if fresh.sub.is_none() {
            fresh.sub = cred.sub.clone();
        }
        // Preserve per-credential proxy / using_api across refresh.
        if fresh.proxy_url.is_none() {
            fresh.proxy_url = cred.proxy_url.clone();
        }
        if fresh.using_api.is_none() {
            fresh.using_api = cred.using_api;
        }
        // Preserve Claude cloak_mode across token refresh.
        if fresh.cloak_mode.is_none() {
            fresh.cloak_mode = cred.cloak_mode.clone();
        }
        // Merge unknown extra keys without wiping using_api/proxy carried in extra.
        for (k, v) in &cred.extra {
            fresh.extra.entry(k.clone()).or_insert_with(|| v.clone());
        }
        Ok(fresh)
    }
}

#[cfg(test)]
mod tests {
    use chrono::Utc;

    use super::*;

    #[tokio::test]
    async fn skips_refresh_when_not_near_expiry() {
        let coord = RefreshCoordinator::new();
        let cred = OAuthCredential {
            provider_type: "claude".into(),
            auth_kind: "oauth".into(),
            access_token: "still-good".into(),
            refresh_token: "rt".into(),
            id_token: None,
            token_type: None,
            expired: Some((Utc::now() + Duration::hours(2)).to_rfc3339()),
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
            cloak_mode: None,
            extra: Default::default(),
        };
        let out = coord.ensure_fresh(cred).await.unwrap();
        assert_eq!(out.access_token, "still-good");
    }
}
