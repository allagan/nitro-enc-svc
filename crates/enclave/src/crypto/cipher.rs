//! AES-256-GCM-SIV encryption and decryption of individual string fields.
//!
//! **Algorithm choice:** AES-256-GCM-SIV (RFC 8452) is nonce-misuse-resistant.
//! Identical plaintext + DEK always produces the same ciphertext (deterministic),
//! which is required for tokenisation/lookup use cases.
//!
//! **Nonce derivation:** The 12-byte nonce is derived deterministically as
//! `HMAC-SHA256(key=DEK, data=plaintext)[0..12]`. Because AES-GCM-SIV is
//! nonce-misuse-resistant, reusing the same nonce for the same plaintext is
//! explicitly safe — it only reveals that two ciphertexts share the same
//! plaintext, which is the tokenisation guarantee we want.
//!
//! **Do NOT substitute plain AES-256-GCM with a fixed nonce.** GCM nonce reuse
//! is catastrophic — it breaks both confidentiality and authentication.

use aes_gcm_siv::{
    aead::{Aead, KeyInit},
    Aes256GcmSiv, Nonce,
};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use hmac::{Hmac, Mac};
use sha2::Sha256;
use thiserror::Error;

/// Byte length of an AES-256 key (32 bytes = 256 bits).
pub const KEY_LEN: usize = 32;

/// Byte length of an AES-GCM-SIV nonce (12 bytes = 96 bits).
pub const NONCE_LEN: usize = 12;

/// Prefix that appears at the start of every encrypted field value.
pub const VERSION_PREFIX: &str = "v1";

/// A parsed, encrypted field value.
///
/// The string representation is `v1.<base64url(nonce)>.<base64url(ciphertext+tag)>`.
// Retained for the future `POST /decrypt` endpoint.
#[allow(dead_code)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EncryptedField {
    /// Raw nonce bytes.
    pub nonce: [u8; NONCE_LEN],
    /// Raw ciphertext + authentication tag bytes.
    pub ciphertext: Vec<u8>,
}

impl EncryptedField {
    /// Encode this value to its canonical string representation.
    pub fn to_string_repr(&self) -> String {
        format!(
            "{}.{}.{}",
            VERSION_PREFIX,
            URL_SAFE_NO_PAD.encode(self.nonce),
            URL_SAFE_NO_PAD.encode(&self.ciphertext),
        )
    }

    /// Parse an encrypted field string back into an [`EncryptedField`].
    ///
    /// # Errors
    ///
    /// Returns [`CipherError::InvalidFormat`] if the string does not match the
    /// expected `v1.<nonce>.<ciphertext>` structure.
    // Retained for the future `POST /decrypt` endpoint.
    #[allow(dead_code)]
    pub fn from_str(s: &str) -> Result<Self, CipherError> {
        let parts: Vec<&str> = s.splitn(3, '.').collect();
        if parts.len() != 3 || parts[0] != VERSION_PREFIX {
            return Err(CipherError::InvalidFormat);
        }
        let nonce_bytes = URL_SAFE_NO_PAD
            .decode(parts[1])
            .map_err(|_| CipherError::InvalidFormat)?;
        if nonce_bytes.len() != NONCE_LEN {
            return Err(CipherError::InvalidFormat);
        }
        let mut nonce = [0u8; NONCE_LEN];
        nonce.copy_from_slice(&nonce_bytes);

        let ciphertext = URL_SAFE_NO_PAD
            .decode(parts[2])
            .map_err(|_| CipherError::InvalidFormat)?;

        Ok(Self { nonce, ciphertext })
    }
}

/// Errors produced by the cipher layer.
#[derive(Debug, Error)]
pub enum CipherError {
    /// The DEK is the wrong length (must be [`KEY_LEN`] bytes).
    #[error("invalid DEK length: expected {KEY_LEN} bytes")]
    InvalidKeyLength,

    /// AES-GCM-SIV encryption or decryption failed.
    #[error("aead operation failed")]
    AeadFailure,

    /// The encrypted field string does not match the expected format.
    // Retained for the future `POST /decrypt` endpoint.
    #[allow(dead_code)]
    #[error("invalid encrypted field format")]
    InvalidFormat,
}

/// Derive a deterministic 12-byte nonce from a DEK and plaintext.
///
/// Uses `HMAC-SHA256(key=DEK, data=plaintext)` and takes the first 12 bytes.
/// This ensures:
/// - Same plaintext + same DEK → same nonce → same ciphertext (tokenisation).
/// - Different plaintext or different DEK → different nonce → different ciphertext.
///
/// AES-GCM-SIV is nonce-misuse-resistant (RFC 8452 §3): reusing the same nonce
/// for the same plaintext is safe and is the intended use case here.
fn derive_nonce(dek: &[u8], plaintext: &[u8]) -> [u8; NONCE_LEN] {
    let mut mac =
        <Hmac<Sha256> as Mac>::new_from_slice(dek).expect("HMAC accepts keys of any length");
    mac.update(plaintext);
    let result = mac.finalize().into_bytes();
    let mut nonce = [0u8; NONCE_LEN];
    nonce.copy_from_slice(&result[..NONCE_LEN]);
    nonce
}

/// Encrypt a plaintext string field using AES-256-GCM-SIV.
///
/// The nonce is derived deterministically via `HMAC-SHA256(key=DEK, data=plaintext)[0..12]`,
/// guaranteeing that identical plaintext + DEK always produces identical ciphertext.
/// This is required for tokenisation and lookup use cases.
///
/// # Errors
///
/// Returns [`CipherError::InvalidKeyLength`] if `dek` is not [`KEY_LEN`] bytes.
/// Returns [`CipherError::AeadFailure`] on an internal AEAD error (unreachable
/// with a valid key and well-formed nonce).
pub fn encrypt_field(plaintext: &[u8], dek: &[u8]) -> Result<EncryptedField, CipherError> {
    let cipher = build_cipher(dek)?;
    let nonce_bytes = derive_nonce(dek, plaintext);
    let nonce = Nonce::from_slice(&nonce_bytes);

    let ciphertext = cipher
        .encrypt(nonce, plaintext)
        .map_err(|_| CipherError::AeadFailure)?;

    Ok(EncryptedField {
        nonce: nonce_bytes,
        ciphertext,
    })
}

/// Decrypt an [`EncryptedField`] back to plaintext bytes.
///
/// # Errors
///
/// Returns [`CipherError::InvalidKeyLength`] if `dek` is not [`KEY_LEN`] bytes.
/// Returns [`CipherError::AeadFailure`] if authentication fails (wrong key or tampered data).
// Retained for the future `POST /decrypt` endpoint.
#[allow(dead_code)]
pub fn decrypt_field(field: &EncryptedField, dek: &[u8]) -> Result<Vec<u8>, CipherError> {
    let cipher = build_cipher(dek)?;
    let nonce = Nonce::from_slice(&field.nonce);
    cipher
        .decrypt(nonce, field.ciphertext.as_ref())
        .map_err(|_| CipherError::AeadFailure)
}

fn build_cipher(dek: &[u8]) -> Result<Aes256GcmSiv, CipherError> {
    if dek.len() != KEY_LEN {
        return Err(CipherError::InvalidKeyLength);
    }
    Aes256GcmSiv::new_from_slice(dek).map_err(|_| CipherError::InvalidKeyLength)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Fixed 32-byte test DEK — never use outside tests.
    fn test_dek_a() -> Vec<u8> {
        vec![0xAAu8; KEY_LEN]
    }

    fn test_dek_b() -> Vec<u8> {
        vec![0xBBu8; KEY_LEN]
    }

    #[test]
    fn encrypt_decrypt_round_trip() {
        let dek = test_dek_a();
        let plaintext = b"123-45-6789";
        let encrypted = encrypt_field(plaintext, &dek).unwrap();
        let decrypted = decrypt_field(&encrypted, &dek).unwrap();
        assert_eq!(decrypted, plaintext);
    }

    #[test]
    fn deterministic_same_plaintext_same_dek() {
        let dek = test_dek_a();
        let plaintext = b"4111111111111111";
        let a = encrypt_field(plaintext, &dek).unwrap();
        let b = encrypt_field(plaintext, &dek).unwrap();
        assert_eq!(a.nonce, b.nonce, "nonces must match for same plaintext+DEK");
        assert_eq!(a.ciphertext, b.ciphertext, "ciphertexts must match");
    }

    #[test]
    fn deterministic_different_plaintext_differs() {
        let dek = test_dek_a();
        let a = encrypt_field(b"4111111111111111", &dek).unwrap();
        let b = encrypt_field(b"5500005555555559", &dek).unwrap();
        assert_ne!(
            a.nonce, b.nonce,
            "different plaintexts must produce different nonces"
        );
        assert_ne!(a.ciphertext, b.ciphertext);
    }

    #[test]
    fn deterministic_different_dek_differs() {
        let plaintext = b"Jane Smith";
        let a = encrypt_field(plaintext, &test_dek_a()).unwrap();
        let b = encrypt_field(plaintext, &test_dek_b()).unwrap();
        assert_ne!(
            a.nonce, b.nonce,
            "different DEKs must produce different nonces"
        );
        assert_ne!(a.ciphertext, b.ciphertext);
    }

    #[test]
    fn wrong_key_fails_decryption() {
        let encrypted = encrypt_field(b"secret", &test_dek_a()).unwrap();
        assert!(decrypt_field(&encrypted, &test_dek_b()).is_err());
    }

    #[test]
    fn invalid_key_length_rejected() {
        let short_key = vec![0u8; 16];
        assert!(encrypt_field(b"x", &short_key).is_err());
    }

    #[test]
    fn string_repr_round_trip() {
        let field = encrypt_field(b"hello", &test_dek_a()).unwrap();
        let s = field.to_string_repr();
        assert!(s.starts_with("v1."));
        let parsed = EncryptedField::from_str(&s).unwrap();
        assert_eq!(parsed.nonce, field.nonce);
        assert_eq!(parsed.ciphertext, field.ciphertext);
    }

    #[test]
    fn from_str_rejects_bad_prefix() {
        assert!(EncryptedField::from_str("v2.abc.def").is_err());
    }

    #[test]
    fn from_str_rejects_too_few_parts() {
        assert!(EncryptedField::from_str("v1.abc").is_err());
    }

    #[test]
    fn from_str_rejects_bad_base64() {
        assert!(EncryptedField::from_str("v1.!!!.abc").is_err());
    }

    #[test]
    fn tampered_ciphertext_fails_auth() {
        let dek = test_dek_a();
        let mut field = encrypt_field(b"tamper me", &dek).unwrap();
        // Flip a byte in the ciphertext to simulate tampering.
        field.ciphertext[0] ^= 0xFF;
        assert!(decrypt_field(&field, &dek).is_err());
    }
}
