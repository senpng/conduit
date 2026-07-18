use async_trait::async_trait;
use base64::Engine as _;
use conduit_ir::error::SecretError;
use keyring::Entry;
use secrecy::{ExposeSecret, SecretVec};
use tracing::{debug, warn};

use crate::backend::{SecretBackend, SecurityLevel};

/// S1 backend: delegates secret storage to the OS keychain.
///
/// All keyring I/O runs on `spawn_blocking` so async callers (and
/// `tokio::time::timeout`) can make progress instead of wedging a worker
/// on a non-yielding Keychain syscall.
pub struct KeychainBackend {
    app_id: String,
}

impl KeychainBackend {
    pub fn try_new(app_id: &str) -> Result<Self, keyring::Error> {
        let probe = Entry::new(app_id, "__probe__")?;
        match probe.get_password() {
            Ok(_) | Err(keyring::Error::NoEntry) => {}
            Err(e) => {
                warn!("OS keychain probe failed: {e}");
                return Err(e);
            }
        }
        debug!("OS keychain available (app_id={app_id})");
        Ok(Self {
            app_id: app_id.to_string(),
        })
    }

    fn service(&self, scope: &str) -> String {
        format!("{}/{}", self.app_id, scope)
    }
}

/// Detects the "item already exists" condition across keyring backends.
///
/// keyring v2 wraps the underlying OS status in `Error::PlatformFailure`
/// (e.g. macOS `errSecDuplicateItem`, whose message is "The specified item
/// already exists in the keychain"). There is no dedicated error variant for
/// it, so we match on the rendered message, which is stable across the
/// platform backends that surface a duplicate on add.
fn is_duplicate_item(err: &keyring::Error) -> bool {
    matches!(err, keyring::Error::PlatformFailure(_))
        && err.to_string().to_ascii_lowercase().contains("already exists")
}

#[async_trait]
impl SecretBackend for KeychainBackend {
    fn security_level(&self) -> SecurityLevel {
        SecurityLevel::Hardware
    }

    async fn put(&self, scope: &str, id: &str, secret: SecretVec<u8>) -> Result<(), SecretError> {
        let svc = self.service(scope);
        let id_owned = id.to_string();
        let encoded = base64::engine::general_purpose::STANDARD.encode(secret.expose_secret());
        let scope_owned = scope.to_string();

        tokio::task::spawn_blocking(move || {
            let entry = Entry::new(&svc, &id_owned)
                .map_err(|e| SecretError::BackendUnavailable(e.to_string()))?;
            match entry.set_password(&encoded) {
                Ok(()) => Ok::<(), SecretError>(()),
                // Some platforms (notably the macOS Keychain) implement
                // `set_password` as an *add* and reject an existing item with
                // `errSecDuplicateItem` instead of updating it. This is the
                // common path for rotating secrets (e.g. OAuth token refresh),
                // so treat it as an update: delete the stale item and re-add.
                Err(e) if is_duplicate_item(&e) => {
                    debug!(
                        scope = %scope_owned,
                        id = %id_owned,
                        "keychain: item exists, replacing"
                    );
                    let _ = entry.delete_password();
                    entry
                        .set_password(&encoded)
                        .map_err(|e| SecretError::PermissionDenied {
                            key: format!("{scope_owned}/{id_owned}"),
                            reason: e.to_string(),
                        })
                }
                Err(e) => Err(SecretError::PermissionDenied {
                    key: format!("{scope_owned}/{id_owned}"),
                    reason: e.to_string(),
                }),
            }
        })
        .await
        .map_err(|e| SecretError::BackendUnavailable(format!("join: {e}")))??;

        debug!(scope, id, "keychain: put");
        Ok(())
    }

    async fn get(&self, scope: &str, id: &str) -> Result<Option<SecretVec<u8>>, SecretError> {
        let svc = self.service(scope);
        let id_owned = id.to_string();
        let key_label = format!("{scope}/{id}");

        let result = tokio::task::spawn_blocking(move || {
            let entry = Entry::new(&svc, &id_owned)
                .map_err(|e| SecretError::BackendUnavailable(e.to_string()))?;
            match entry.get_password() {
                Ok(encoded) => {
                    let bytes = base64::engine::general_purpose::STANDARD
                        .decode(&encoded)
                        .map_err(|e| SecretError::Serialization(e.to_string()))?;
                    Ok::<Option<Vec<u8>>, SecretError>(Some(bytes))
                }
                Err(keyring::Error::NoEntry) => Ok(None),
                Err(e) => Err(SecretError::PermissionDenied {
                    key: key_label,
                    reason: e.to_string(),
                }),
            }
        })
        .await
        .map_err(|e| SecretError::BackendUnavailable(format!("join: {e}")))??;

        match result {
            Some(bytes) => {
                debug!(scope, id, "keychain: get hit");
                Ok(Some(SecretVec::new(bytes)))
            }
            None => {
                debug!(scope, id, "keychain: get miss");
                Ok(None)
            }
        }
    }

    async fn delete(&self, scope: &str, id: &str) -> Result<(), SecretError> {
        let svc = self.service(scope);
        let id_owned = id.to_string();
        let key_label = format!("{scope}/{id}");

        tokio::task::spawn_blocking(move || {
            let entry = Entry::new(&svc, &id_owned)
                .map_err(|e| SecretError::BackendUnavailable(e.to_string()))?;
            match entry.delete_password() {
                Ok(()) => Ok::<(), SecretError>(()),
                Err(keyring::Error::NoEntry) => Ok(()),
                Err(e) => Err(SecretError::PermissionDenied {
                    key: key_label,
                    reason: e.to_string(),
                }),
            }
        })
        .await
        .map_err(|e| SecretError::BackendUnavailable(format!("join: {e}")))??;

        debug!(scope, id, "keychain: delete");
        Ok(())
    }
}
