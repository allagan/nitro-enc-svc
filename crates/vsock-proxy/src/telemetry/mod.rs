//! Telemetry initialisation for the vsock-proxy sidecar.
//!
//! The proxy uses a lightweight setup: structured JSON logs only.
//! No OTLP export — the proxy runs in the EKS pod, not inside the enclave.

use anyhow::Result;
use tracing_subscriber::EnvFilter;

/// Initialise the tracing subscriber for the vsock-proxy sidecar.
///
/// Outputs structured JSON logs to stdout at the configured log level.
///
/// # Errors
///
/// Returns an error if the subscriber has already been set.
pub fn init(log_level: &str) -> Result<()> {
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new(log_level));

    tracing_subscriber::fmt()
        .json()
        .with_env_filter(filter)
        .try_init()
        .map_err(|e| anyhow::anyhow!("failed to initialise vsock-proxy tracing subscriber: {e}"))
}
