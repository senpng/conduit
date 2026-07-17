use async_trait::async_trait;
use conduit_ir::error::SecretError;
use secrecy::SecretVec;

/// The security tier of a secret backend.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SecurityLevel {
    /// S1: OS keychain — hardware-backed where available (Secure Enclave on macOS,
    /// Windows DPAPI + Credential Manager, libsecret on Linux).
    Hardware,
    /// S2: AES-256-GCM encrypted file, key derived from a user-provided master password
    /// via Argon2id.
    MasterPassword,
}

/// Core secret-storage contract.  All implementations must be `Send + Sync` for
/// use across async task boundaries.
#[async_trait]
pub trait SecretBackend: Send + Sync {
    /// The security tier this backend provides.
    fn security_level(&self) -> SecurityLevel;

    /// Store (or overwrite) a secret.
    ///
    /// `scope` groups secrets by logical namespace (e.g. `"upstream_key"`).
    /// `id` is the unique identifier within that scope.
    async fn put(&self, scope: &str, id: &str, secret: SecretVec<u8>) -> Result<(), SecretError>;

    /// Retrieve a secret.  Returns `None` when the entry does not exist.
    async fn get(&self, scope: &str, id: &str) -> Result<Option<SecretVec<u8>>, SecretError>;

    /// Permanently delete a secret.  Succeeds even if the entry was absent.
    async fn delete(&self, scope: &str, id: &str) -> Result<(), SecretError>;

    /// Atomically replace a secret with `new_secret`.
    ///
    /// The default implementation is a `put` over the existing entry; backends
    /// that can provide true atomicity should override this.
    async fn rotate_secret(
        &self,
        scope: &str,
        id: &str,
        new_secret: SecretVec<u8>,
    ) -> Result<(), SecretError> {
        self.put(scope, id, new_secret).await
    }
}
