//! PKCE (RFC 7636) helpers: S256 code_verifier / code_challenge.

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use rand::RngCore;
use sha2::{Digest, Sha256};

use crate::error::OAuthError;

/// PKCE code verifier + S256 challenge pair.
#[derive(Debug, Clone)]
pub struct PkceCodes {
    pub code_verifier: String,
    pub code_challenge: String,
}

/// Generate a PKCE pair (96 random bytes → 128-char base64url verifier).
pub fn generate_pkce() -> Result<PkceCodes, OAuthError> {
    let mut bytes = [0u8; 96];
    rand::thread_rng().fill_bytes(&mut bytes);
    let code_verifier = URL_SAFE_NO_PAD.encode(bytes);
    let code_challenge = s256_challenge(&code_verifier);
    Ok(PkceCodes {
        code_verifier,
        code_challenge,
    })
}

/// SHA-256(code_verifier) as base64url without padding.
pub fn s256_challenge(code_verifier: &str) -> String {
    let hash = Sha256::digest(code_verifier.as_bytes());
    URL_SAFE_NO_PAD.encode(hash)
}

/// Cryptographically random hex state (32 bytes → 64 hex chars).
pub fn generate_state() -> String {
    let mut bytes = [0u8; 16];
    rand::thread_rng().fill_bytes(&mut bytes);
    hex::encode(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pkce_verifier_length_and_challenge() {
        let pkce = generate_pkce().unwrap();
        assert!(pkce.code_verifier.len() >= 43);
        assert_eq!(pkce.code_challenge, s256_challenge(&pkce.code_verifier));
        // challenge is base64url without padding
        assert!(!pkce.code_challenge.contains('='));
        assert!(!pkce.code_challenge.contains('+'));
    }

    #[test]
    fn state_is_unique_hex() {
        let a = generate_state();
        let b = generate_state();
        assert_eq!(a.len(), 32);
        assert_ne!(a, b);
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
    }
}
