//! TLS listener setup using rustls with ACM for Nitro Enclaves certificates.
//!
//! The certificate and private key are delivered to the enclave over vsock by
//! the ACM for Nitro Enclaves integration running on the parent EC2 instance.
//! This module loads them and constructs a `rustls::ServerConfig`.

use anyhow::{Context, Result};
use rustls::ServerConfig;
use std::sync::Arc;

/// Build a [`rustls::ServerConfig`] from PEM-encoded certificate and private key bytes.
///
/// The bytes are typically loaded from the filesystem paths written by the
/// ACM for Nitro Enclaves agent on the parent EC2 instance.
///
/// # Errors
///
/// Returns an error if the certificate or key cannot be parsed, or if rustls
/// rejects the configuration.
pub fn build_server_config(cert_pem: &[u8], key_pem: &[u8]) -> Result<Arc<ServerConfig>> {
    let certs = rustls_pemfile::certs(&mut std::io::BufReader::new(cert_pem))
        .collect::<Result<Vec<_>, _>>()
        .context("failed to parse TLS certificate chain")?;

    let key = rustls_pemfile::private_key(&mut std::io::BufReader::new(key_pem))
        .context("failed to read TLS private key")?
        .context("no private key found in PEM data")?;

    let config = ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(certs, key)
        .context("failed to build rustls ServerConfig")?;

    Ok(Arc::new(config))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_empty_cert_pem() {
        let result = build_server_config(b"", b"");
        assert!(result.is_err());
    }

    #[test]
    fn rejects_garbage_pem() {
        let result = build_server_config(b"not a pem", b"also not a pem");
        assert!(result.is_err());
    }
}
