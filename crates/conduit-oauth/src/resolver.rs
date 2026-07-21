//! Resolve provider secrets into access tokens (API key or OAuth with refresh).

use std::sync::Arc;

use async_trait::async_trait;
use secrecy::{ExposeSecret, SecretString, SecretVec};

use crate::{
    credential::{
        oauth_extra_headers, AuthMode, OAuthCredential, OAuthProviderKind, ResolvedCredential,
    },
    error::OAuthError,
    refresh::RefreshCoordinator,
};

/// Abstraction over secret storage used by the credential resolver.
#[async_trait]
pub trait SecretStore: Send + Sync {
    async fn get(&self, scope: &str, id: &str) -> Result<Option<SecretVec<u8>>, OAuthError>;
    async fn put(&self, scope: &str, id: &str, secret: SecretVec<u8>) -> Result<(), OAuthError>;
}

/// Resolve upstream credentials, refreshing OAuth tokens when near expiry.
pub struct CredentialResolver {
    store: Arc<dyn SecretStore>,
    refresh: RefreshCoordinator,
    scope: String,
}

impl CredentialResolver {
    pub fn new(store: Arc<dyn SecretStore>) -> Self {
        Self {
            store,
            refresh: RefreshCoordinator::new(),
            scope: "upstream_key".into(),
        }
    }

    pub fn with_refresh(mut self, refresh: RefreshCoordinator) -> Self {
        self.refresh = refresh;
        self
    }

    /// Daemon config proxy (CLIProxyAPI `cfg.ProxyURL`); env / per-cred still apply.
    pub fn with_default_proxy(mut self, proxy: Option<String>) -> Self {
        self.refresh = std::mem::take(&mut self.refresh).with_default_proxy(proxy);
        self
    }

    /// Resolve a secret id into an access token + auth mode for upstream calls.
    pub async fn resolve(&self, key_id: &str) -> Result<ResolvedCredential, OAuthError> {
        tracing::debug!(key_id, "credential resolve: loading secret");
        let bytes = self
            .store
            .get(&self.scope, key_id)
            .await?
            .ok_or_else(|| OAuthError::Credential(format!("secret not found: {key_id}")))?;

        let raw = bytes.expose_secret();
        if let Some(cred) = OAuthCredential::try_parse_secret(raw) {
            tracing::debug!(
                key_id,
                provider = %cred.provider_type,
                needs_refresh = cred.needs_refresh(self.refresh.lead()),
                "credential resolve: oauth bundle"
            );
            return self.resolve_oauth(key_id, cred).await;
        }

        // Raw API key
        let key = String::from_utf8_lossy(raw).trim().to_string();
        if key.is_empty() {
            return Err(OAuthError::Credential("empty secret".into()));
        }
        tracing::debug!(key_id, "credential resolve: raw api key");
        Ok(ResolvedCredential {
            access_token: SecretString::new(key),
            auth_mode: AuthMode::ApiKey,
            extra_headers: vec![],
            label: None,
            using_api: false,
        })
    }

    async fn resolve_oauth(
        &self,
        key_id: &str,
        cred: OAuthCredential,
    ) -> Result<ResolvedCredential, OAuthError> {
        let kind = cred.kind().unwrap_or(OAuthProviderKind::Claude);
        let needed = cred.needs_refresh(self.refresh.lead());
        let fresh = if needed {
            tracing::info!(key_id, provider = %kind, "oauth access token near expiry; refreshing");
            self.refresh.refresh(cred).await?
        } else {
            cred
        };

        // Persist only when we actually refreshed.
        if needed {
            if let Ok(bytes) = fresh.to_json_bytes() {
                let _ = self
                    .store
                    .put(&self.scope, key_id, SecretVec::new(bytes))
                    .await;
            }
        }

        let headers = oauth_extra_headers(kind, &fresh);
        let label = fresh.email.clone();
        let using_api = fresh.using_api();
        Ok(ResolvedCredential {
            access_token: SecretString::new(fresh.access_token),
            auth_mode: AuthMode::OAuth(kind),
            extra_headers: headers,
            label,
            using_api,
        })
    }

    /// Force-refresh and rewrite the stored credential.
    pub async fn force_refresh(&self, key_id: &str) -> Result<OAuthCredential, OAuthError> {
        let bytes = self
            .store
            .get(&self.scope, key_id)
            .await?
            .ok_or_else(|| OAuthError::Credential(format!("secret not found: {key_id}")))?;
        let cred = OAuthCredential::try_parse_secret(bytes.expose_secret())
            .ok_or_else(|| OAuthError::Credential("not an oauth credential".into()))?;
        let fresh = self.refresh.refresh(cred).await?;
        self.store
            .put(&self.scope, key_id, SecretVec::new(fresh.to_json_bytes()?))
            .await?;
        Ok(fresh)
    }

    pub async fn store_credential(
        &self,
        key_id: &str,
        cred: &OAuthCredential,
    ) -> Result<(), OAuthError> {
        self.store
            .put(&self.scope, key_id, SecretVec::new(cred.to_json_bytes()?))
            .await
    }
}

/// In-memory secret store for tests.
#[cfg(test)]
pub struct MemorySecretStore {
    map: parking_lot::Mutex<std::collections::HashMap<String, Vec<u8>>>,
}

#[cfg(test)]
impl Default for MemorySecretStore {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
impl MemorySecretStore {
    pub fn new() -> Self {
        Self {
            map: parking_lot::Mutex::new(std::collections::HashMap::new()),
        }
    }
}

#[cfg(test)]
#[async_trait]
impl SecretStore for MemorySecretStore {
    async fn get(&self, scope: &str, id: &str) -> Result<Option<SecretVec<u8>>, OAuthError> {
        let key = format!("{scope}/{id}");
        Ok(self.map.lock().get(&key).map(|v| SecretVec::new(v.clone())))
    }

    async fn put(&self, scope: &str, id: &str, secret: SecretVec<u8>) -> Result<(), OAuthError> {
        let key = format!("{scope}/{id}");
        self.map.lock().insert(key, secret.expose_secret().clone());
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use chrono::{Duration, Utc};

    use super::*;

    #[tokio::test]
    async fn resolve_raw_api_key() {
        let store = Arc::new(MemorySecretStore::new());
        store
            .put("upstream_key", "p1", SecretVec::new(b"sk-test".to_vec()))
            .await
            .unwrap();
        let r = CredentialResolver::new(store).resolve("p1").await.unwrap();
        assert_eq!(r.auth_mode, AuthMode::ApiKey);
        assert_eq!(r.access_token.expose_secret(), "sk-test");
    }

    #[tokio::test]
    async fn resolve_oauth_not_expired() {
        let store = Arc::new(MemorySecretStore::new());
        let cred = OAuthCredential {
            provider_type: "claude".into(),
            auth_kind: "oauth".into(),
            access_token: "oauth-at".into(),
            refresh_token: "rt".into(),
            id_token: None,
            token_type: None,
            expired: Some((Utc::now() + Duration::hours(2)).to_rfc3339()),
            last_refresh: None,
            email: Some("u@c.com".into()),
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
        store
            .put(
                "upstream_key",
                "p2",
                SecretVec::new(cred.to_json_bytes().unwrap()),
            )
            .await
            .unwrap();
        let r = CredentialResolver::new(store).resolve("p2").await.unwrap();
        assert!(matches!(
            r.auth_mode,
            AuthMode::OAuth(OAuthProviderKind::Claude)
        ));
        assert_eq!(r.access_token.expose_secret(), "oauth-at");
        // Claude betas/fingerprint headers are applied per-request by conduit-upstream
        // claude_oauth relay (not stored on the credential resolve path).
        // Default cloak_mode=auto → no synthetic header (avoids header noise).
        assert!(
            r.extra_headers.is_empty(),
            "claude oauth auto cloak should not emit x-conduit-cloak-mode"
        );
        assert!(!r.using_api);
    }
}
