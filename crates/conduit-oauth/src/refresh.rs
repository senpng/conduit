//! Token refresh with per-refresh-token singleflight.

use std::{collections::HashMap, sync::Arc};

use chrono::Duration;
use parking_lot::Mutex;
use tokio::sync::Mutex as AsyncMutex;

use crate::{
    credential::{OAuthCredential, OAuthProviderKind},
    error::OAuthError,
    providers::{ClaudeOAuth, CodexOAuth, GrokOAuth},
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
}

impl RefreshCoordinator {
    pub fn new() -> Self {
        Self {
            inflight: Arc::new(Mutex::new(HashMap::new())),
            lead: default_refresh_lead(),
        }
    }

    pub fn with_lead(mut self, lead: Duration) -> Self {
        self.lead = lead;
        self
    }

    pub fn lead(&self) -> Duration {
        self.lead
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
        let mut fresh = match kind {
            OAuthProviderKind::Claude => ClaudeOAuth::new().refresh(&cred.refresh_token).await?,
            OAuthProviderKind::Codex => CodexOAuth::new().refresh(&cred.refresh_token).await?,
            OAuthProviderKind::Xai => {
                GrokOAuth::new()
                    .refresh(&cred.refresh_token, cred.token_endpoint.as_deref())
                    .await?
            }
        };
        // Preserve base_url / email if refresh response omitted them
        if fresh.base_url.is_none() {
            fresh.base_url = cred.base_url.clone();
        }
        if fresh.email.is_none() {
            fresh.email = cred.email.clone();
        }
        if fresh.account_id.is_none() {
            fresh.account_id = cred.account_id.clone();
        }
        if fresh.token_endpoint.is_none() {
            fresh.token_endpoint = cred.token_endpoint.clone();
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
            sub: None,
            base_url: None,
            token_endpoint: None,
            extra: Default::default(),
        };
        let out = coord.ensure_fresh(cred).await.unwrap();
        assert_eq!(out.access_token, "still-good");
    }
}
