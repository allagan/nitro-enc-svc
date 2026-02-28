//! Configuration loading and validation for the enclave service.
//!
//! All values are read from environment variables at startup. The process will
//! exit with a clear error message if any required variable is missing or invalid.

use anyhow::{Context, Result};
use serde::Deserialize;

/// Validated enclave service configuration.
#[derive(Debug, Clone, Deserialize)]
pub struct Config {
    /// Secrets Manager ARN of the envelope-encrypted DEK. **Required.**
    pub secret_arn: String,

    /// KMS key ID used to decrypt the DEK. **Required.**
    pub kms_key_id: String,

    /// S3 bucket containing OpenAPI spec files. **Required.**
    pub s3_bucket: String,

    /// S3 key prefix for OpenAPI spec files.
    #[serde(default = "default_s3_prefix")]
    pub s3_prefix: String,

    /// HTTP header used to identify which schema to apply.
    #[serde(default = "default_schema_header")]
    pub schema_header_name: String,

    /// How often (seconds) to re-fetch and rotate the cached DEK.
    #[serde(default = "default_dek_rotation_interval")]
    pub dek_rotation_interval_secs: u64,

    /// How often (seconds) to refresh the cached OpenAPI schemas from S3.
    #[serde(default = "default_schema_refresh_interval")]
    pub schema_refresh_interval_secs: u64,

    /// Vsock CID of the parent EC2 aws-vsock-proxy. **Required.**
    pub vsock_proxy_cid: u32,

    /// Vsock port of the aws-vsock-proxy.
    #[serde(default = "default_vsock_proxy_port")]
    pub vsock_proxy_port: u32,

    /// Port the enclave HTTPS server listens on.
    #[serde(default = "default_tls_port")]
    pub tls_port: u16,

    /// Filesystem path to the PEM-encoded TLS certificate chain delivered by
    /// ACM for Nitro Enclaves. **Required.**
    pub tls_cert_path: String,

    /// Filesystem path to the PEM-encoded TLS private key delivered by
    /// ACM for Nitro Enclaves. **Required.**
    pub tls_key_path: String,

    /// OTLP endpoint (vsock address to OTEL collector). **Required.**
    pub otel_exporter_otlp_endpoint: String,

    /// Tracing log level (e.g. `"info"`, `"debug"`).
    #[serde(default = "default_log_level")]
    pub log_level: String,
}

fn default_s3_prefix() -> String {
    "schemas/".into()
}
fn default_schema_header() -> String {
    "X-Schema-Name".into()
}
fn default_dek_rotation_interval() -> u64 {
    3600
}
fn default_schema_refresh_interval() -> u64 {
    300
}
fn default_vsock_proxy_port() -> u32 {
    8000
}
fn default_tls_port() -> u16 {
    443
}
fn default_log_level() -> String {
    "info".into()
}

impl Config {
    /// Load and validate configuration from environment variables.
    ///
    /// # Errors
    ///
    /// Returns an error if any required variable is absent or cannot be parsed.
    pub fn from_env() -> Result<Self> {
        let cfg = config::Config::builder()
            .add_source(config::Environment::default())
            .build()
            .context("failed to build configuration from environment")?;

        let c: Config = cfg
            .try_deserialize()
            .context("failed to deserialise configuration")?;

        c.validate()?;
        Ok(c)
    }

    /// Validate all fields, returning a descriptive error on the first failure.
    fn validate(&self) -> Result<()> {
        ensure_non_empty(&self.secret_arn, "SECRET_ARN")?;
        ensure_non_empty(&self.kms_key_id, "KMS_KEY_ID")?;
        ensure_non_empty(&self.s3_bucket, "S3_BUCKET")?;
        ensure_non_empty(&self.otel_exporter_otlp_endpoint, "OTEL_EXPORTER_OTLP_ENDPOINT")?;
        ensure_non_empty(&self.tls_cert_path, "TLS_CERT_PATH")?;
        ensure_non_empty(&self.tls_key_path, "TLS_KEY_PATH")?;

        if self.vsock_proxy_cid == 0 {
            anyhow::bail!("VSOCK_PROXY_CID must be a non-zero vsock CID");
        }
        if self.dek_rotation_interval_secs == 0 {
            anyhow::bail!("DEK_ROTATION_INTERVAL_SECS must be > 0");
        }
        if self.schema_refresh_interval_secs == 0 {
            anyhow::bail!("SCHEMA_REFRESH_INTERVAL_SECS must be > 0");
        }
        Ok(())
    }
}

fn ensure_non_empty(value: &str, name: &str) -> Result<()> {
    if value.trim().is_empty() {
        anyhow::bail!("{name} is required and must not be empty");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_correct() {
        assert_eq!(default_s3_prefix(), "schemas/");
        assert_eq!(default_schema_header(), "X-Schema-Name");
        assert_eq!(default_dek_rotation_interval(), 3600);
        assert_eq!(default_schema_refresh_interval(), 300);
        assert_eq!(default_vsock_proxy_port(), 8000);
        assert_eq!(default_tls_port(), 443);
        assert_eq!(default_log_level(), "info");
    }

    #[test]
    fn validate_rejects_empty_secret_arn() {
        let cfg = Config {
            secret_arn: "".into(),
            kms_key_id: "key".into(),
            s3_bucket: "bucket".into(),
            s3_prefix: default_s3_prefix(),
            schema_header_name: default_schema_header(),
            dek_rotation_interval_secs: default_dek_rotation_interval(),
            schema_refresh_interval_secs: default_schema_refresh_interval(),
            vsock_proxy_cid: 3,
            vsock_proxy_port: default_vsock_proxy_port(),
            tls_port: default_tls_port(),
            tls_cert_path: "/run/acm/tls.crt".into(),
            tls_key_path: "/run/acm/tls.key".into(),
            otel_exporter_otlp_endpoint: "vsock://3:4317".into(),
            log_level: default_log_level(),
        };
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn validate_rejects_zero_cid() {
        let cfg = Config {
            secret_arn: "arn".into(),
            kms_key_id: "key".into(),
            s3_bucket: "bucket".into(),
            s3_prefix: default_s3_prefix(),
            schema_header_name: default_schema_header(),
            dek_rotation_interval_secs: default_dek_rotation_interval(),
            schema_refresh_interval_secs: default_schema_refresh_interval(),
            vsock_proxy_cid: 0,
            vsock_proxy_port: default_vsock_proxy_port(),
            tls_port: default_tls_port(),
            tls_cert_path: "/run/acm/tls.crt".into(),
            tls_key_path: "/run/acm/tls.key".into(),
            otel_exporter_otlp_endpoint: "vsock://3:4317".into(),
            log_level: default_log_level(),
        };
        assert!(cfg.validate().is_err());
    }
}
