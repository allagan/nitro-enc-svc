//! AES-256-GCM-SIV field encryption primitives.
//!
//! This module is intentionally free of AWS and HTTP dependencies.
//! It provides the low-level encrypt/decrypt operations used by the DEK layer.
//!
//! # Ciphertext format
//!
//! ```text
//! v1.<base64url-no-pad(nonce)>.<base64url-no-pad(ciphertext+tag)>
//! ```
//!
//! The `v1` prefix enables future algorithm or key-version migration without
//! breaking existing ciphertext.

pub mod cipher;

pub use cipher::KEY_LEN;
