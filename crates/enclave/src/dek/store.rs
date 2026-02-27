//! [`DekStore`]: thread-safe cache for the decrypted Data Encryption Key.

use std::sync::Arc;
use thiserror::Error;
use tokio::sync::RwLock;

use crate::crypto::KEY_LEN;

/// Errors produced by the DEK layer.
#[derive(Debug, Error)]
pub enum DekError {
    /// The DEK has not yet been fetched and decrypted.
    #[error("DEK not yet initialised")]
    NotInitialised,

    /// The decrypted key material has an unexpected length.
    #[error("DEK has invalid length: expected {KEY_LEN} bytes, got {0}")]
    InvalidLength(usize),
}

/// Fixed-size key buffer that holds exactly [`KEY_LEN`] bytes.
///
/// Stored inside [`DekStore`]; cloned into handler call stacks when needed.
/// When this type is dropped, the memory is overwritten with zeroes to
/// minimise the window during which plaintext key material lives in RAM.
#[derive(Clone)]
pub struct DekBytes(pub Box<[u8; KEY_LEN]>);

impl Drop for DekBytes {
    fn drop(&mut self) {
        // Zero the key material on drop.
        self.0.iter_mut().for_each(|b| *b = 0);
    }
}

impl std::fmt::Debug for DekBytes {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Never print key material — not even in debug builds.
        f.write_str("DekBytes([REDACTED])")
    }
}

/// Thread-safe store for the current Data Encryption Key.
///
/// Wraps an `Arc<RwLock<Option<DekBytes>>>` so that:
/// - Many concurrent read-lock holders (request handlers) can access the DEK
///   simultaneously without contention.
/// - A single write-lock holder (the background rotation task) can atomically
///   swap in a new key without blocking readers for more than a microsecond.
#[derive(Clone, Debug)]
pub struct DekStore {
    inner: Arc<RwLock<Option<DekBytes>>>,
}

impl DekStore {
    /// Create a new, empty [`DekStore`].
    pub fn new() -> Self {
        Self {
            inner: Arc::new(RwLock::new(None)),
        }
    }

    /// Returns `true` if a DEK is currently cached.
    pub async fn is_ready(&self) -> bool {
        self.inner.read().await.is_some()
    }

    /// Store (or replace) the current DEK.
    ///
    /// The provided `key_bytes` slice must be exactly [`KEY_LEN`] bytes.
    ///
    /// # Errors
    ///
    /// Returns [`DekError::InvalidLength`] if the slice has the wrong length.
    pub async fn store(&self, key_bytes: &[u8]) -> Result<(), DekError> {
        if key_bytes.len() != KEY_LEN {
            return Err(DekError::InvalidLength(key_bytes.len()));
        }
        let mut buf = Box::new([0u8; KEY_LEN]);
        buf.copy_from_slice(key_bytes);
        let mut lock = self.inner.write().await;
        *lock = Some(DekBytes(buf));
        Ok(())
    }

    /// Borrow a clone of the current DEK bytes.
    ///
    /// The clone is a short-lived copy; callers should use and drop it promptly.
    ///
    /// # Errors
    ///
    /// Returns [`DekError::NotInitialised`] if no DEK has been stored yet.
    pub async fn current(&self) -> Result<DekBytes, DekError> {
        let lock = self.inner.read().await;
        lock.as_ref().cloned().ok_or(DekError::NotInitialised)
    }
}

impl Default for DekStore {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn initially_not_ready() {
        let store = DekStore::new();
        assert!(!store.is_ready().await);
        assert!(store.current().await.is_err());
    }

    #[tokio::test]
    async fn store_and_retrieve() {
        let store = DekStore::new();
        let key = vec![0x42u8; KEY_LEN];
        store.store(&key).await.unwrap();
        assert!(store.is_ready().await);
        let retrieved = store.current().await.unwrap();
        assert_eq!(&retrieved.0[..], key.as_slice());
    }

    #[tokio::test]
    async fn rejects_wrong_length() {
        let store = DekStore::new();
        let result = store.store(&[0u8; 16]).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn rotation_replaces_key() {
        let store = DekStore::new();
        let key1 = vec![0x01u8; KEY_LEN];
        let key2 = vec![0x02u8; KEY_LEN];
        store.store(&key1).await.unwrap();
        store.store(&key2).await.unwrap();
        let current = store.current().await.unwrap();
        assert_eq!(&current.0[..], key2.as_slice());
    }

    #[test]
    fn dek_bytes_redacted_in_debug() {
        let mut buf = Box::new([0u8; KEY_LEN]);
        buf[0] = 0xFF;
        let dek = DekBytes(buf);
        assert!(format!("{dek:?}").contains("REDACTED"));
    }
}
