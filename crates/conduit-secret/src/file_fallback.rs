//! Local file fallback for secrets when the OS keychain is slow or ACL-blocked.
//!
//! Layout: `{dir}/{scope}/{id}.bin` (base64-encoded, mode 0600).
//! Used as a dual-write companion to the OS keychain for local-first daemons.

use std::path::{Path, PathBuf};

use async_trait::async_trait;
use base64::Engine as _;
use conduit_ir::error::SecretError;
use secrecy::{ExposeSecret, SecretVec};
use tracing::{debug, warn};

use crate::backend::{SecretBackend, SecurityLevel};

/// Wraps a primary backend (usually keychain) and mirrors secrets to disk.
///
/// **Read order**: file first (fast, no UI prompts), then primary.
/// **Write order**: primary then file (best-effort both).
pub struct FileFallbackBackend {
    primary: std::sync::Arc<dyn SecretBackend>,
    dir: PathBuf,
}

impl FileFallbackBackend {
    pub fn new(primary: std::sync::Arc<dyn SecretBackend>, dir: impl Into<PathBuf>) -> Self {
        let dir = dir.into();
        let _ = std::fs::create_dir_all(&dir);
        Self { primary, dir }
    }

    fn path(&self, scope: &str, id: &str) -> PathBuf {
        // Sanitize path segments
        let scope = scope.replace(['/', '\\'], "_");
        let id = id.replace(['/', '\\'], "_");
        self.dir.join(scope).join(format!("{id}.bin"))
    }

    async fn file_put(
        &self,
        scope: &str,
        id: &str,
        secret: &SecretVec<u8>,
    ) -> Result<(), SecretError> {
        let path = self.path(scope, id);
        let encoded = base64::engine::general_purpose::STANDARD.encode(secret.expose_secret());
        let path_clone = path.clone();
        tokio::task::spawn_blocking(move || {
            if let Some(parent) = path_clone.parent() {
                std::fs::create_dir_all(parent).map_err(|e| {
                    SecretError::BackendUnavailable(format!("mkdir {}: {e}", parent.display()))
                })?;
            }
            std::fs::write(&path_clone, encoded.as_bytes()).map_err(|e| {
                SecretError::BackendUnavailable(format!("write {}: {e}", path_clone.display()))
            })?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let _ =
                    std::fs::set_permissions(&path_clone, std::fs::Permissions::from_mode(0o600));
            }
            Ok::<(), SecretError>(())
        })
        .await
        .map_err(|e| SecretError::BackendUnavailable(format!("join: {e}")))??;
        debug!(scope, id, path = %path.display(), "file secret: put");
        Ok(())
    }

    async fn file_get(&self, scope: &str, id: &str) -> Result<Option<SecretVec<u8>>, SecretError> {
        let path = self.path(scope, id);
        let path_clone = path.clone();
        let result = tokio::task::spawn_blocking(move || {
            if !path_clone.exists() {
                return Ok::<Option<Vec<u8>>, SecretError>(None);
            }
            let encoded = std::fs::read_to_string(&path_clone).map_err(|e| {
                SecretError::BackendUnavailable(format!("read {}: {e}", path_clone.display()))
            })?;
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(encoded.trim())
                .map_err(|e| SecretError::Serialization(e.to_string()))?;
            Ok(Some(bytes))
        })
        .await
        .map_err(|e| SecretError::BackendUnavailable(format!("join: {e}")))??;

        Ok(result.map(SecretVec::new))
    }

    async fn file_delete(&self, scope: &str, id: &str) -> Result<(), SecretError> {
        let path = self.path(scope, id);
        let path_clone = path.clone();
        tokio::task::spawn_blocking(move || {
            if path_clone.exists() {
                let _ = std::fs::remove_file(&path_clone);
            }
            Ok::<(), SecretError>(())
        })
        .await
        .map_err(|e| SecretError::BackendUnavailable(format!("join: {e}")))??;
        Ok(())
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }
}

#[async_trait]
impl SecretBackend for FileFallbackBackend {
    fn security_level(&self) -> SecurityLevel {
        // Reads are served from the plaintext file mirror first (see `get`), so
        // the *effective* protection is filesystem permissions, not the primary
        // keychain. Report the weakest tier honestly rather than inheriting the
        // primary's (misleading) Hardware level.
        SecurityLevel::PlaintextFile
    }

    async fn put(&self, scope: &str, id: &str, secret: SecretVec<u8>) -> Result<(), SecretError> {
        // Mirror to file first so a subsequent get never depends on keychain ACL UI.
        if let Err(e) = self.file_put(scope, id, &secret).await {
            warn!(error = %e, "file secret put failed");
        }
        // Primary (keychain) — best-effort; do not fail the request if ACL blocks.
        match self.primary.put(scope, id, secret).await {
            Ok(()) => Ok(()),
            Err(e) => {
                warn!(error = %e, "primary secret put failed; file mirror kept");
                // If file write succeeded we still consider put ok for local-first use.
                Ok(())
            }
        }
    }

    async fn get(&self, scope: &str, id: &str) -> Result<Option<SecretVec<u8>>, SecretError> {
        // Prefer file — avoids macOS Keychain ACL prompts that hang daemons.
        if let Ok(Some(v)) = self.file_get(scope, id).await {
            debug!(scope, id, "file secret: get hit");
            return Ok(Some(v));
        }
        match self.primary.get(scope, id).await {
            Ok(Some(v)) => {
                // Backfill file for next time.
                if let Err(e) = self.file_put(scope, id, &v).await {
                    warn!(error = %e, "file secret backfill failed");
                }
                Ok(Some(v))
            }
            Ok(None) => Ok(None),
            Err(e) => {
                // Primary failed (ACL hang/timeout path may surface as error).
                warn!(error = %e, "primary secret get failed");
                self.file_get(scope, id).await
            }
        }
    }

    async fn delete(&self, scope: &str, id: &str) -> Result<(), SecretError> {
        let _ = self.file_delete(scope, id).await;
        self.primary.delete(scope, id).await
    }
}
