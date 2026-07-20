//! In-memory cache decorator for [`SecretBackend`].
//!
//! Hot-path `get` avoids re-running Argon2id + AES decrypt on every request.
//! Writes (`put` / `rotate_secret` / `delete`) update or drop the entry so the
//! cache never serves a stale secret after a local mutation.

use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

use async_trait::async_trait;
use conduit_ir::error::SecretError;
use secrecy::{ExposeSecret, SecretVec};
use tracing::debug;

use crate::backend::{SecretBackend, SecurityLevel};

type CacheKey = (String, String);

/// Process-local plaintext cache in front of a durable [`SecretBackend`].
///
/// Secrets remain encrypted at rest; only the decrypted payload is held in
/// memory for the daemon lifetime (or until invalidated by a write).
pub struct CachingSecretBackend {
    inner: Arc<dyn SecretBackend>,
    /// `(scope, id)` → decrypted bytes. `SecretVec` zeroizes on drop/remove.
    cache: Mutex<HashMap<CacheKey, SecretVec<u8>>>,
}

impl CachingSecretBackend {
    pub fn new(inner: Arc<dyn SecretBackend>) -> Self {
        Self {
            inner,
            cache: Mutex::new(HashMap::new()),
        }
    }

    /// Wrap `inner` and return as a trait object.
    pub fn wrap(inner: Arc<dyn SecretBackend>) -> Arc<dyn SecretBackend> {
        Arc::new(Self::new(inner))
    }

    fn lock_cache(
        &self,
    ) -> std::sync::MutexGuard<'_, HashMap<CacheKey, SecretVec<u8>>> {
        self.cache
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn cache_key(scope: &str, id: &str) -> CacheKey {
        (scope.to_string(), id.to_string())
    }
}

#[async_trait]
impl SecretBackend for CachingSecretBackend {
    fn security_level(&self) -> SecurityLevel {
        self.inner.security_level()
    }

    async fn put(&self, scope: &str, id: &str, secret: SecretVec<u8>) -> Result<(), SecretError> {
        let plaintext = secret.expose_secret().clone();
        self.inner
            .put(scope, id, SecretVec::new(plaintext.clone()))
            .await?;
        self.lock_cache().insert(
            Self::cache_key(scope, id),
            SecretVec::new(plaintext),
        );
        debug!(scope, id, "secret_cache: put (cached)");
        Ok(())
    }

    async fn get(&self, scope: &str, id: &str) -> Result<Option<SecretVec<u8>>, SecretError> {
        {
            let guard = self.lock_cache();
            if let Some(cached) = guard.get(&Self::cache_key(scope, id)) {
                debug!(scope, id, "secret_cache: hit");
                return Ok(Some(SecretVec::new(cached.expose_secret().clone())));
            }
        }

        debug!(scope, id, "secret_cache: miss");
        let got = self.inner.get(scope, id).await?;
        if let Some(ref secret) = got {
            self.lock_cache().insert(
                Self::cache_key(scope, id),
                SecretVec::new(secret.expose_secret().clone()),
            );
        }
        Ok(got)
    }

    async fn delete(&self, scope: &str, id: &str) -> Result<(), SecretError> {
        self.inner.delete(scope, id).await?;
        self.lock_cache().remove(&Self::cache_key(scope, id));
        debug!(scope, id, "secret_cache: delete (invalidated)");
        Ok(())
    }

    async fn rotate_secret(
        &self,
        scope: &str,
        id: &str,
        new_secret: SecretVec<u8>,
    ) -> Result<(), SecretError> {
        // Prefer put so both durable store and cache stay consistent in one path.
        self.put(scope, id, new_secret).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::master_password::MasterPasswordBackend;
    use secrecy::SecretString;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tempfile::tempdir;

    /// Counts `get` calls to prove cache hits skip the inner backend.
    struct CountingBackend {
        inner: MasterPasswordBackend,
        gets: AtomicUsize,
        puts: AtomicUsize,
    }

    impl CountingBackend {
        fn new(inner: MasterPasswordBackend) -> Self {
            Self {
                inner,
                gets: AtomicUsize::new(0),
                puts: AtomicUsize::new(0),
            }
        }
    }

    #[async_trait]
    impl SecretBackend for CountingBackend {
        fn security_level(&self) -> SecurityLevel {
            self.inner.security_level()
        }

        async fn put(
            &self,
            scope: &str,
            id: &str,
            secret: SecretVec<u8>,
        ) -> Result<(), SecretError> {
            self.puts.fetch_add(1, Ordering::SeqCst);
            self.inner.put(scope, id, secret).await
        }

        async fn get(
            &self,
            scope: &str,
            id: &str,
        ) -> Result<Option<SecretVec<u8>>, SecretError> {
            self.gets.fetch_add(1, Ordering::SeqCst);
            self.inner.get(scope, id).await
        }

        async fn delete(&self, scope: &str, id: &str) -> Result<(), SecretError> {
            self.inner.delete(scope, id).await
        }
    }

    fn counting(dir: &std::path::Path) -> Arc<CountingBackend> {
        Arc::new(CountingBackend::new(MasterPasswordBackend::new(
            SecretString::new("cache-test-password".into()),
            dir,
        )))
    }

    #[tokio::test]
    async fn second_get_is_cache_hit() {
        let dir = tempdir().unwrap();
        let inner = counting(dir.path());
        let cached = CachingSecretBackend::new(inner.clone());

        cached
            .put("upstream_key", "p1", SecretVec::new(b"sk-secret".to_vec()))
            .await
            .unwrap();
        // put went through inner once; get should not need decrypt after put-cache.
        assert_eq!(inner.gets.load(Ordering::SeqCst), 0);

        let a = cached.get("upstream_key", "p1").await.unwrap().unwrap();
        assert_eq!(a.expose_secret(), b"sk-secret");
        assert_eq!(inner.gets.load(Ordering::SeqCst), 0, "served from put cache");

        let b = cached.get("upstream_key", "p1").await.unwrap().unwrap();
        assert_eq!(b.expose_secret(), b"sk-secret");
        assert_eq!(inner.gets.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn miss_loads_once_then_hits() {
        let dir = tempdir().unwrap();
        let raw = MasterPasswordBackend::new(
            SecretString::new("cache-test-password".into()),
            dir.path(),
        );
        raw.put("upstream_key", "p2", SecretVec::new(b"from-disk".to_vec()))
            .await
            .unwrap();

        let inner = counting(dir.path());
        let cached = CachingSecretBackend::new(inner.clone());

        let first = cached.get("upstream_key", "p2").await.unwrap().unwrap();
        assert_eq!(first.expose_secret(), b"from-disk");
        assert_eq!(inner.gets.load(Ordering::SeqCst), 1);

        let second = cached.get("upstream_key", "p2").await.unwrap().unwrap();
        assert_eq!(second.expose_secret(), b"from-disk");
        assert_eq!(inner.gets.load(Ordering::SeqCst), 1, "second get must hit cache");
    }

    #[tokio::test]
    async fn put_overwrites_cached_value() {
        let dir = tempdir().unwrap();
        let inner = counting(dir.path());
        let cached = CachingSecretBackend::new(inner);

        cached
            .put("s", "id", SecretVec::new(b"old".to_vec()))
            .await
            .unwrap();
        cached
            .put("s", "id", SecretVec::new(b"new".to_vec()))
            .await
            .unwrap();

        let got = cached.get("s", "id").await.unwrap().unwrap();
        assert_eq!(got.expose_secret(), b"new");
    }

    #[tokio::test]
    async fn delete_invalidates_cache() {
        let dir = tempdir().unwrap();
        let inner = counting(dir.path());
        let cached = CachingSecretBackend::new(inner.clone());

        cached
            .put("s", "id", SecretVec::new(b"gone".to_vec()))
            .await
            .unwrap();
        cached.delete("s", "id").await.unwrap();

        assert!(cached.get("s", "id").await.unwrap().is_none());
        // miss after delete → one inner get that returns None
        assert_eq!(inner.gets.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn rotate_updates_cache() {
        let dir = tempdir().unwrap();
        let inner = counting(dir.path());
        let cached = CachingSecretBackend::new(inner.clone());

        cached
            .put("s", "id", SecretVec::new(b"v1".to_vec()))
            .await
            .unwrap();
        cached
            .rotate_secret("s", "id", SecretVec::new(b"v2".to_vec()))
            .await
            .unwrap();

        let got = cached.get("s", "id").await.unwrap().unwrap();
        assert_eq!(got.expose_secret(), b"v2");
        assert_eq!(inner.gets.load(Ordering::SeqCst), 0);
    }
}
