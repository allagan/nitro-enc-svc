//! `vsock-proxy` — EKS sidecar binary entry point.
//!
//! Startup sequence:
//! 1. Load and validate [`Config`] from environment variables.
//! 2. Initialise structured JSON logging.
//! 3. Start the TCP accept loop, proxying each connection to the enclave vsock port.

mod config;
mod proxy;
mod telemetry;

use anyhow::Result;

#[tokio::main]
async fn main() -> Result<()> {
    // -----------------------------------------------------------------------
    // 1. Configuration
    // -----------------------------------------------------------------------
    let cfg = config::Config::from_env().map_err(|e| {
        eprintln!("ERROR: vsock-proxy configuration invalid: {e}");
        e
    })?;

    // -----------------------------------------------------------------------
    // 2. Telemetry
    // -----------------------------------------------------------------------
    telemetry::init(&cfg.log_level)?;

    // -----------------------------------------------------------------------
    // 3. Proxy
    // -----------------------------------------------------------------------
    proxy::run(&cfg).await
}
