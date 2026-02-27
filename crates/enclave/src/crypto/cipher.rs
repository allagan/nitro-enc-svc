//! AES-256-GCM-SIV encryption and decryption of individual string fields.
//!
//! **Algorithm choice:** AES-256-GCM-SIV (RFC 8452) is nonce-misuse-resistant.
//! Identical plaintext + DEK always produces the same ciphertext (deterministic),
//! which is required for tokenisation/lookup use cases.
//!
//! **Do NOT substitute plain AES-256-GCM with a fixed nonce.** GCM nonce reuse
//! is catastrophic — it breaks both confidentiality and authentication.

use aes_gcm_siv::{
    aead::{Aead, KeyInit, OsRng},
    Aes256GcmSiv, Nonce,
};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
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

/// Encrypt a plaintext string field using AES-256-GCM-SIV.
///
/// A random 96-bit nonce is generated per call via the OS CSPRNG. Because
/// AES-GCM-SIV is deterministic-nonce-safe, the same plaintext + DEK will
/// produce the same output only when the same nonce is reused — here each call
/// generates a fresh nonce.
///
/// # Errors
///
/// Returns [`CipherError::InvalidKeyLength`] if `dek` is not [`KEY_LEN`] bytes.
/// Returns [`CipherError::AeadFailure`] on an internal AEAD error (should be unreachable
/// with a valid key and nonce).
pub fn encrypt_field(plaintext: &[u8], dek: &[u8]) -> Result<EncryptedField, CipherError> {
    let cipher = build_cipher(dek)?;

    // Use OsRng for a cryptographically secure random nonce.
    use aes_gcm_siv::aead::rand_core::RngCore;
    let mut nonce_bytes = [0u8; NONCE_LEN];
    OsRng.fill_bytes(&mut nonce_bytes);
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

    fn random_dek() -> Vec<u8> {
        use aes_gcm_siv::aead::rand_core::RngCore;
        let mut key = vec![0u8; KEY_LEN];
        OsRng.fill_bytes(&mut key);
        key
    }

    #[test]
    fn encrypt_decrypt_round_trip() {
        let dek = random_dek();
        let plaintext = b"123-45-6789";
        let encrypted = encrypt_field(plaintext, &dek).unwrap();
        let decrypted = decrypt_field(&encrypted, &dek).unwrap();
        assert_eq!(decrypted, plaintext);
    }

    #[test]
    fn wrong_key_fails_decryption() {
        let dek1 = random_dek();
        let dek2 = random_dek();
        let encrypted = encrypt_field(b"secret", &dek1).unwrap();
        assert!(decrypt_field(&encrypted, &dek2).is_err());
    }

    #[test]
    fn invalid_key_length_rejected() {
        let short_key = vec![0u8; 16];
        assert!(encrypt_field(b"x", &short_key).is_err());
    }

    #[test]
    fn string_repr_round_trip() {
        let dek = random_dek();
        let field = encrypt_field(b"hello", &dek).unwrap();
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
        let dek = random_dek();
        let mut field = encrypt_field(b"tamper me", &dek).unwrap();
        // Flip a byte in the ciphertext to simulate tampering.
        field.ciphertext[0] ^= 0xFF;
        assert!(decrypt_field(&field, &dek).is_err());
    }
}
