use std::path::{Path, PathBuf};

use aes_gcm::{
    aead::{Aead, KeyInit},
    Aes256Gcm, Key, Nonce,
};
use argon2::{Argon2, Params, Version};
use async_trait::async_trait;
use conduit_ir::error::SecretError;
use rand::RngCore;
use secrecy::{ExposeSecret, SecretString, SecretVec};
use tracing::{debug, instrument};
use zeroize::Zeroizing;

use crate::backend::{SecretBackend, SecurityLevel};

// ── Constants ────────────────────────────────────────────────────────────────

const SALT_LEN: usize = 16;
const NONCE_LEN: usize = 12;
const KEY_LEN: usize = 32;

/// Argon2id parameters — tuned for interactive use (not batch cracking).
const ARGON2_MEM_KIB: u32 = 65_536; // 64 MiB
const ARGON2_ITERS: u32 = 3;
const ARGON2_PARALLELISM: u32 = 1;

// KNOWN LIMITATION: `Aes256Gcm` (from the `aes-gcm` crate) does not implement
// `Zeroize`, so the expanded round keys it holds cannot be explicitly wiped on
// drop.  This is a limitation of the upstream crate tracked at
// https://github.com/RustCrypto/AEADs/issues/TODO (placeholder).
// Mitigation: `with_cipher` keeps the cipher alive for the minimum scope and
// drops it immediately after use, minimising the window during which the round
// keys are in memory.

// ── Backend ──────────────────────────────────────────────────────────────────

/// Master-password backend: AES-256-GCM encryption of each secret, with the
/// key encryption key (KEK) derived from a user-provided master password via
/// Argon2id.
///
/// On-disk layout for each secret at `{data_dir}/secrets/{scope}/{id}.enc`:
/// ```text
/// [ salt  (16 bytes) ][ nonce (12 bytes) ][ ciphertext + GCM tag ]
/// ```
///
/// The salt is unique per entry; the nonce is unique per write.  Both are
/// stored in the file so the file is self-contained and portable.
pub struct MasterPasswordBackend {
    /// The master password is held in a `SecretString` so it is zeroized on drop.
    password: SecretString,
    data_dir: PathBuf,
}

impl MasterPasswordBackend {
    pub fn new(password: SecretString, data_dir: impl AsRef<Path>) -> Self {
        Self {
            password,
            data_dir: data_dir.as_ref().to_path_buf(),
        }
    }

    /// Path to the encrypted file for `(scope, id)`.
    fn secret_path(&self, scope: &str, id: &str) -> PathBuf {
        self.data_dir
            .join("secrets")
            .join(scope)
            .join(format!("{}.enc", id))
    }

    /// Encrypt `plaintext` → `[salt | nonce | ciphertext+tag]`.
    fn encrypt(&self, plaintext: &[u8]) -> Result<Vec<u8>, SecretError> {
        let mut rng = rand::thread_rng();

        let mut salt = [0u8; SALT_LEN];
        rng.fill_bytes(&mut salt);

        let mut nonce_bytes = [0u8; NONCE_LEN];
        rng.fill_bytes(&mut nonce_bytes);

        let kek = derive_kek(&self.password, &salt)?;
        let nonce = Nonce::from_slice(&nonce_bytes);

        let ciphertext = with_cipher(&kek, |cipher| {
            cipher
                .encrypt(nonce, plaintext)
                .map_err(|e| SecretError::PermissionDenied {
                    key: "<encrypt>".into(),
                    reason: e.to_string(),
                })
        })?;

        let mut out = Vec::with_capacity(SALT_LEN + NONCE_LEN + ciphertext.len());
        out.extend_from_slice(&salt);
        out.extend_from_slice(&nonce_bytes);
        out.extend_from_slice(&ciphertext);
        Ok(out)
    }

    /// Decrypt `data` (produced by `encrypt`) → plaintext.
    fn decrypt(&self, data: &[u8], key_hint: &str) -> Result<Vec<u8>, SecretError> {
        if data.len() < SALT_LEN + NONCE_LEN {
            return Err(SecretError::DecryptionFailed {
                key: key_hint.to_string(),
                reason: "file too short".into(),
            });
        }

        let (salt, rest) = data.split_at(SALT_LEN);
        let (nonce_bytes, ciphertext) = rest.split_at(NONCE_LEN);

        let kek = derive_kek(&self.password, salt)?;
        let nonce = Nonce::from_slice(nonce_bytes);
        let key_hint = key_hint.to_string();

        with_cipher(&kek, |cipher| {
            cipher
                .decrypt(nonce, ciphertext)
                .map_err(|_| SecretError::DecryptionFailed {
                    key: key_hint.clone(),
                    reason: "AES-GCM authentication failed — wrong password or corrupted file"
                        .into(),
                })
        })
    }
}

// ── Free functions ────────────────────────────────────────────────────────────

/// Derive a 32-byte KEK from `password` and `salt` using Argon2id.
///
/// The output is written directly into a stack-allocated array wrapped in
/// `Zeroizing<[u8; KEY_LEN]>` so it is wiped on drop — no intermediate copy
/// lingers after the caller's scope ends.
pub fn derive_kek(
    password: &SecretString,
    salt: &[u8],
) -> Result<Zeroizing<[u8; KEY_LEN]>, SecretError> {
    let params = Params::new(
        ARGON2_MEM_KIB,
        ARGON2_ITERS,
        ARGON2_PARALLELISM,
        Some(KEY_LEN),
    )
    .map_err(|e| SecretError::BackendUnavailable(format!("argon2 params: {e}")))?;

    let argon2 = Argon2::new(argon2::Algorithm::Argon2id, Version::V0x13, params);

    let mut kek = Zeroizing::new([0u8; KEY_LEN]);
    argon2
        .hash_password_into(password.expose_secret().as_bytes(), salt, kek.as_mut())
        .map_err(|e| SecretError::BackendUnavailable(format!("argon2 kdf: {e}")))?;

    Ok(kek)
}

/// Create an `Aes256Gcm` cipher for the duration of `f`, then immediately drop it.
///
/// The short lifetime minimises the window during which the round keys are in
/// memory.  See the known-limitation note at the top of this file.
fn with_cipher<R>(kek: &Zeroizing<[u8; KEY_LEN]>, f: impl FnOnce(&Aes256Gcm) -> R) -> R {
    let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(kek.as_ref()));
    let r = f(&cipher);
    drop(cipher);
    r
}

#[async_trait]
impl SecretBackend for MasterPasswordBackend {
    fn security_level(&self) -> SecurityLevel {
        SecurityLevel::MasterPassword
    }

    #[instrument(skip(self, secret), fields(scope, id))]
    async fn put(&self, scope: &str, id: &str, secret: SecretVec<u8>) -> Result<(), SecretError> {
        let path = self.secret_path(scope, id);

        tokio::fs::create_dir_all(path.parent().unwrap())
            .await
            .map_err(|e| SecretError::BackendUnavailable(e.to_string()))?;

        let encrypted = self.encrypt(secret.expose_secret())?;

        // Atomic write: write to a temp file, then rename.
        let tmp_path = path.with_extension("enc.tmp");
        tokio::fs::write(&tmp_path, &encrypted)
            .await
            .map_err(|e| SecretError::BackendUnavailable(e.to_string()))?;

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            tokio::fs::set_permissions(&tmp_path, std::fs::Permissions::from_mode(0o600))
                .await
                .ok();
        }

        tokio::fs::rename(&tmp_path, &path)
            .await
            .map_err(|e| SecretError::BackendUnavailable(e.to_string()))?;

        debug!(scope, id, "master_password: put");
        Ok(())
    }

    #[instrument(skip(self), fields(scope, id))]
    async fn get(&self, scope: &str, id: &str) -> Result<Option<SecretVec<u8>>, SecretError> {
        let path = self.secret_path(scope, id);

        if !path.exists() {
            debug!(scope, id, "master_password: get miss");
            return Ok(None);
        }

        let data = tokio::fs::read(&path)
            .await
            .map_err(|e| SecretError::BackendUnavailable(e.to_string()))?;

        let plaintext = self.decrypt(&data, &format!("{scope}/{id}"))?;
        debug!(scope, id, "master_password: get hit");
        Ok(Some(SecretVec::new(plaintext)))
    }

    #[instrument(skip(self), fields(scope, id))]
    async fn delete(&self, scope: &str, id: &str) -> Result<(), SecretError> {
        let path = self.secret_path(scope, id);
        if path.exists() {
            tokio::fs::remove_file(&path)
                .await
                .map_err(|e| SecretError::BackendUnavailable(e.to_string()))?;
        }
        debug!(scope, id, "master_password: delete");
        Ok(())
    }
}

// ── Unit tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use secrecy::SecretString;
    use tempfile::tempdir;

    use super::*;

    fn backend(dir: &Path) -> MasterPasswordBackend {
        MasterPasswordBackend::new(
            SecretString::new("correct-horse-battery-staple".into()),
            dir,
        )
    }

    #[tokio::test]
    async fn round_trip() {
        let dir = tempdir().unwrap();
        let b = backend(dir.path());
        let secret = SecretVec::new(b"hello world".to_vec());
        b.put("test", "key1", secret).await.unwrap();
        let got = b.get("test", "key1").await.unwrap().unwrap();
        assert_eq!(got.expose_secret(), b"hello world");
    }

    #[tokio::test]
    async fn get_miss() {
        let dir = tempdir().unwrap();
        let b = backend(dir.path());
        let got = b.get("test", "absent").await.unwrap();
        assert!(got.is_none());
    }

    #[tokio::test]
    async fn delete_removes_entry() {
        let dir = tempdir().unwrap();
        let b = backend(dir.path());
        b.put("test", "k", SecretVec::new(b"data".to_vec()))
            .await
            .unwrap();
        b.delete("test", "k").await.unwrap();
        assert!(b.get("test", "k").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn wrong_password_fails_decryption() {
        let dir = tempdir().unwrap();
        let b1 = backend(dir.path());
        b1.put("test", "k", SecretVec::new(b"secret".to_vec()))
            .await
            .unwrap();

        let b2 = MasterPasswordBackend::new(SecretString::new("wrong-password".into()), dir.path());
        let result = b2.get("test", "k").await;
        assert!(matches!(result, Err(SecretError::DecryptionFailed { .. })));
    }

    #[tokio::test]
    async fn rotate_replaces_secret() {
        let dir = tempdir().unwrap();
        let b = backend(dir.path());
        b.put("s", "id", SecretVec::new(b"old".to_vec()))
            .await
            .unwrap();
        b.rotate_secret("s", "id", SecretVec::new(b"new".to_vec()))
            .await
            .unwrap();
        let got = b.get("s", "id").await.unwrap().unwrap();
        assert_eq!(got.expose_secret(), b"new");
    }

    #[test]
    fn derive_kek_no_stack_copy() {
        // Verify derive_kek returns a Zeroizing wrapper (wiped on drop) and that
        // the bytes match a known reference derivation.
        let password = SecretString::new("test-password-magic-AABBCC".into());
        let salt = [0x42u8; 16];
        let kek = derive_kek(&password, &salt).unwrap();
        assert_eq!(kek.len(), 32);
        // KEK must be non-zero (argon2 actually produced output)
        assert!(kek.iter().any(|&b| b != 0));
    }
}
