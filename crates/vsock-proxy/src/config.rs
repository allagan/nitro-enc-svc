//! Configuration loading and validation for the vsock-proxy sidecar.

use anyhow::{Context, Result};
use serde::Deserialize;

/// Validated vsock-proxy configuration.
#[derive(Debug, Clone, Deserialize)]
pub struct Config {
    /// TCP port to accept incoming HTTPS connections from the NLB.
    #[serde(default = "default_listen_port")]
    pub listen_port: u16,

    /// Vsock CID of the Nitro Enclave on this node. **Required.**
    pub enclave_cid: u32,

    /// Vsock port the enclave TLS server listens on.
    #[serde(default = "default_enclave_port")]
    pub enclave_port: u32,

    /// Address of the main app container (pod-local HTTP, e.g. `"127.0.0.1:8080"`).
    /// **Required.**
    pub main_app_addr: String,

    /// Tracing log level.
    #[serde(default = "default_log_level")]
    pub log_level: String,
}

fn default_listen_port() -> u16 {
    8443
}
fn default_enclave_port() -> u32 {
    443
}
fn default_log_level() -> String {
    "info".into()
}

impl Config {
    /// Load and validate configuration from environment variables.
    pub fn from_env() -> Result<Self> {
        let cfg = config::Config::builder()
            .add_source(config::Environment::default())
            .build()
            .context("failed to build vsock-proxy configuration")?;

        let c: Config = cfg
            .try_deserialize()
            .context("failed to deserialise vsock-proxy configuration")?;

        c.validate()?;
        Ok(c)
    }

    fn validate(&self) -> Result<()> {
        if self.enclave_cid == 0 {
            anyhow::bail!("ENCLAVE_CID must be a non-zero vsock CID");
        }
        if self.main_app_addr.trim().is_empty() {
            anyhow::bail!("MAIN_APP_ADDR is required and must not be empty");
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults() {
        assert_eq!(default_listen_port(), 8443);
        assert_eq!(default_enclave_port(), 443);
        assert_eq!(default_log_level(), "info");
    }

    #[test]
    fn validate_rejects_zero_cid() {
        let cfg = Config {
            listen_port: 8443,
            enclave_cid: 0,
            enclave_port: 443,
            main_app_addr: "127.0.0.1:8080".into(),
            log_level: "info".into(),
        };
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn validate_rejects_empty_main_app_addr() {
        let cfg = Config {
            listen_port: 8443,
            enclave_cid: 16,
            enclave_port: 443,
            main_app_addr: "  ".into(),
            log_level: "info".into(),
        };
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn validate_accepts_valid_config() {
        let cfg = Config {
            listen_port: 8443,
            enclave_cid: 16,
            enclave_port: 443,
            main_app_addr: "127.0.0.1:8080".into(),
            log_level: "info".into(),
        };
        assert!(cfg.validate().is_ok());
    }
}
