pub mod audit;
pub mod backend;
pub mod file_fallback;
pub mod keychain;
pub mod master_password;

use std::{path::Path, sync::Arc};

pub use audit::{AuditAction, AuditEntry, SecretAuditLog};
pub use backend::{SecretBackend, SecurityLevel};
pub use conduit_ir::error::SecretError;
pub use file_fallback::FileFallbackBackend;
pub use keychain::KeychainBackend;
pub use master_password::MasterPasswordBackend;

/// The result of `build_backend`.
///
/// Always contains a ready-to-use backend.  When the system had to fall back
/// from the OS keychain (S1) to master-password encryption (S2) the
/// `downgrade_warning` field is `Some(message)`.  **Callers MUST display this
/// warning to the user before first use.**
pub struct BackendResult {
    pub backend: Arc<dyn SecretBackend>,
    pub downgrade_warning: Option<String>,
}

/// Build the best available secret backend for the current platform.
///
/// Strategy:
/// 1. Attempt S1 (OS keychain via `KeychainBackend`).
/// 2. If S1 is unavailable, create an S2 (`MasterPasswordBackend`) and return
///    a downgrade warning that must be shown to the user.
///
/// `app_id` is used as the keychain service namespace (e.g. `"conduit"`).
/// `app_dir` is the application data directory; S2 stores encrypted files
/// under `{app_dir}/secrets/`.
/// `master_password` is only consulted when S1 is unavailable; it is
/// consumed and stored as a `secrecy::SecretString`.
pub async fn build_backend(
    app_id: &str,
    app_dir: &Path,
    master_password: Option<secrecy::SecretString>,
) -> BackendResult {
    // ── S1: OS keychain + local file mirror (avoids macOS ACL hangs) ─────────
    match KeychainBackend::try_new(app_id) {
        Ok(b) => {
            tracing::info!("secret backend: OS keychain (S1) + file mirror under secrets/");
            let mirrored = FileFallbackBackend::new(Arc::new(b), app_dir.join("secrets"));
            return BackendResult {
                backend: Arc::new(mirrored),
                downgrade_warning: None,
            };
        }
        Err(e) => {
            tracing::warn!(
                "OS keychain unavailable ({}); falling back to S2 (master password)",
                e
            );
        }
    }

    // ── S2: master-password AES-256-GCM ─────────────────────────────────────
    let password = master_password.unwrap_or_else(|| {
        // Provide an empty password as last resort so the process can start,
        // but the warning message makes it clear the user must supply one.
        secrecy::SecretString::new(String::new())
    });

    let b = MasterPasswordBackend::new(password, app_dir);

    let warning = concat!(
        "WARNING: The OS keychain is unavailable on this system.\n",
        "Secrets are stored in an AES-256-GCM encrypted file protected by your master password.\n",
        "If you have not set a strong master password, your API keys are at risk.\n",
        "To use the OS keychain, ensure the secret service daemon is running and try again."
    )
    .to_string();

    tracing::warn!("{}", warning);

    BackendResult {
        backend: Arc::new(b),
        downgrade_warning: Some(warning),
    }
}
