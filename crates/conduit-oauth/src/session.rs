//! In-process OAuth session tracking (pending login flows).

use std::{
    collections::HashMap,
    sync::Arc,
    time::{Duration, Instant},
};

use parking_lot::Mutex;
use serde::Serialize;

use crate::{credential::OAuthProviderKind, error::OAuthError, pkce::PkceCodes};

/// How long an OAuth login session stays pending before auto-expiry.
pub const SESSION_TTL: Duration = Duration::from_secs(15 * 60);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum SessionStatus {
    Pending,
    Completed,
    Error,
    Cancelled,
}

/// Public (non-secret) view of an OAuth session for console/CLI polling.
#[derive(Debug, Clone, Serialize)]
pub struct SessionView {
    pub session_id: String,
    pub kind: String,
    pub status: SessionStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auth_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user_code: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub verification_uri: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub verification_uri_complete: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires_in: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone)]
pub struct OAuthSession {
    pub id: String,
    pub kind: OAuthProviderKind,
    pub status: SessionStatus,
    pub created_at: Instant,
    /// Display name for the provider row once completed.
    pub name: Option<String>,
    /// Re-auth against an existing provider id (overwrite secret).
    pub provider_id: Option<String>,
    pub state: Option<String>,
    pub pkce: Option<PkceCodes>,
    pub auth_url: Option<String>,
    // Device code fields (Grok)
    pub user_code: Option<String>,
    pub device_code: Option<String>,
    pub verification_uri: Option<String>,
    pub verification_uri_complete: Option<String>,
    pub device_expires_in: Option<u64>,
    pub poll_interval_secs: Option<u64>,
    pub token_endpoint: Option<String>,
    // Completion
    pub completed_provider_id: Option<String>,
    pub email: Option<String>,
    pub error: Option<String>,
}

impl OAuthSession {
    pub fn view(&self) -> SessionView {
        SessionView {
            session_id: self.id.clone(),
            kind: self.kind.as_str().to_string(),
            status: self.status,
            auth_url: self.auth_url.clone(),
            user_code: self.user_code.clone(),
            verification_uri: self.verification_uri.clone(),
            verification_uri_complete: self.verification_uri_complete.clone(),
            expires_in: self.device_expires_in,
            provider_id: self.completed_provider_id.clone(),
            email: self.email.clone(),
            error: self.error.clone(),
        }
    }

    pub fn is_expired(&self) -> bool {
        self.created_at.elapsed() > SESSION_TTL
    }
}

/// Thread-safe in-memory session store.
#[derive(Clone, Default)]
pub struct SessionStore {
    inner: Arc<Mutex<HashMap<String, OAuthSession>>>,
}

impl SessionStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert(&self, session: OAuthSession) {
        self.inner.lock().insert(session.id.clone(), session);
    }

    pub fn get(&self, id: &str) -> Option<OAuthSession> {
        self.inner.lock().get(id).cloned()
    }

    pub fn update<F>(&self, id: &str, f: F) -> Result<OAuthSession, OAuthError>
    where
        F: FnOnce(&mut OAuthSession),
    {
        let mut map = self.inner.lock();
        let s = map
            .get_mut(id)
            .ok_or_else(|| OAuthError::SessionNotFound(id.to_string()))?;
        f(s);
        Ok(s.clone())
    }

    pub fn remove(&self, id: &str) -> Option<OAuthSession> {
        self.inner.lock().remove(id)
    }

    pub fn find_by_state(&self, state: &str) -> Option<OAuthSession> {
        self.inner
            .lock()
            .values()
            .find(|s| s.state.as_deref() == Some(state))
            .cloned()
    }

    /// Drop completed/error/cancelled or TTL-expired sessions.
    pub fn gc(&self) {
        let mut map = self.inner.lock();
        map.retain(|_, s| !s.is_expired() && matches!(s.status, SessionStatus::Pending));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn store_roundtrip() {
        let store = SessionStore::new();
        let id = "s1".to_string();
        store.insert(OAuthSession {
            id: id.clone(),
            kind: OAuthProviderKind::Claude,
            status: SessionStatus::Pending,
            created_at: Instant::now(),
            name: None,
            provider_id: None,
            state: Some("st".into()),
            pkce: None,
            auth_url: Some("https://example".into()),
            user_code: None,
            device_code: None,
            verification_uri: None,
            verification_uri_complete: None,
            device_expires_in: None,
            poll_interval_secs: None,
            token_endpoint: None,
            completed_provider_id: None,
            email: None,
            error: None,
        });
        assert!(store.find_by_state("st").is_some());
        store
            .update(&id, |s| s.status = SessionStatus::Completed)
            .unwrap();
        assert_eq!(store.get(&id).unwrap().status, SessionStatus::Completed);
    }
}
