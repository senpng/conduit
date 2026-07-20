pub mod audit;
pub mod backend;
pub mod cache;
pub mod master_password;

use std::{path::Path, sync::Arc};

pub use audit::{AuditAction, AuditEntry, SecretAuditLog};
pub use backend::{SecretBackend, SecurityLevel};
pub use cache::CachingSecretBackend;
pub use conduit_ir::error::SecretError;
pub use master_password::MasterPasswordBackend;
use secrecy::{ExposeSecret, SecretString};

/// The result of [`build_backend`].
///
/// Always contains a ready-to-use master-password backend.  When no password
/// was supplied (empty string), `warning` explains the risk and **callers
/// SHOULD surface it** before first use.
pub struct BackendResult {
    pub backend: Arc<dyn SecretBackend>,
    pub warning: Option<String>,
}

/// Build the secret backend: AES-256-GCM files under `{app_dir}/secrets/`,
/// keyed by Argon2id from the master password, wrapped with an in-process
/// plaintext cache so hot-path `get` does not re-run Argon2 on every request.
///
/// `master_password` is stored as a `SecretString` for the process lifetime.
/// An empty / missing password is accepted so the daemon can start in local
/// dev, but a warning is returned — production deployments should set
/// `CONDUIT_MASTER_PASSWORD`.
pub fn build_backend(app_dir: &Path, master_password: Option<SecretString>) -> BackendResult {
    let password = master_password.unwrap_or_else(|| SecretString::new(String::new()));
    let empty = password.expose_secret().is_empty();

    let durable = Arc::new(MasterPasswordBackend::new(password, app_dir));
    let backend = CachingSecretBackend::wrap(durable);

    // Do not log here — callers (e.g. conduitd) own presentation so the
    // message is not duplicated across crate boundaries.
    let warning = if empty {
        Some(
            concat!(
                "WARNING: No master password set (CONDUIT_MASTER_PASSWORD / --master-password).\n",
                "Secrets are AES-256-GCM encrypted under `secrets/`, but the key is derived from an ",
                "empty password — anyone who can read the files can decrypt them offline.\n",
                "Set a strong master password before storing production API keys."
            )
            .to_string(),
        )
    } else {
        tracing::info!(
            "secret backend: master-password AES-256-GCM under secrets/ (in-memory get cache)"
        );
        None
    };

    BackendResult { backend, warning }
}
