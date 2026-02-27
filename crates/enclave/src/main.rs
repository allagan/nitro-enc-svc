//! `nitro-enc-svc` — enclave binary entry point.
//!
//! Startup sequence:
//! 1. Load and validate [`Config`] from environment variables.
//! 2. Initialise the telemetry pipeline (OTEL + tracing).
//! 3. Initialise AWS SDK clients pointing at the vsock proxy.
//! 4. Fetch + decrypt the DEK from Secrets Manager / KMS and seed [`DekStore`].
//! 5. Load OpenAPI schemas from S3 into [`SchemaCache`].
//! 6. Spawn background tasks: DEK rotation, schema refresh.
//! 7. Build the Axum router and start the TLS server.

mod aws;
mod config;
mod crypto;
mod dek;
mod schema;
mod server;
mod telemetry;

use anyhow::Result;
use tracing::info;

use config::Config;
use dek::DekStore;
use schema::SchemaCache;
use server::state::AppState;

#[tokio::main]
async fn main() -> Result<()> {
    // -----------------------------------------------------------------------
    // 1. Configuration
    // -----------------------------------------------------------------------
    let cfg = Config::from_env().map_err(|e| {
        // Telemetry is not yet up; write to stderr directly.
        eprintln!("ERROR: configuration invalid: {e}");
        e
    })?;

    // -----------------------------------------------------------------------
    // 2. Telemetry
    // -----------------------------------------------------------------------
    telemetry::init_telemetry(&cfg.otel_exporter_otlp_endpoint, &cfg.log_level)?;
    info!(
        version = env!("CARGO_PKG_VERSION"),
        tls_port = cfg.tls_port,
        "nitro-enc-svc starting"
    );

    // -----------------------------------------------------------------------
    // 3. AWS clients
    // -----------------------------------------------------------------------
    let aws = aws::AwsClients::init(cfg.vsock_proxy_cid, cfg.vsock_proxy_port).await?;

    // -----------------------------------------------------------------------
    // 4. DEK initialisation
    // -----------------------------------------------------------------------
    let dek_store = DekStore::new();
    dek::fetch_and_store(&aws, &cfg, &dek_store).await?;

    // -----------------------------------------------------------------------
    // 5. Schema cache initialisation
    // -----------------------------------------------------------------------
    let schema_cache = SchemaCache::new();
    schema::load_all(&aws, &cfg, &schema_cache).await?;

    // -----------------------------------------------------------------------
    // 6. Background tasks
    // -----------------------------------------------------------------------
    let _dek_rotation = dek::rotation_task(aws.clone(), cfg.clone(), dek_store.clone());
    let _schema_refresh = schema::refresh_task(aws.clone(), cfg.clone(), schema_cache.clone());

    // -----------------------------------------------------------------------
    // 7. HTTP server
    // -----------------------------------------------------------------------
    let state = AppState::new(dek_store, schema_cache, cfg.schema_header_name.clone());
    let router = server::router::build(state);

    let addr: std::net::SocketAddr = ([0, 0, 0, 0], cfg.tls_port).into();
    info!(addr = %addr, "listening");

    // TODO: wrap listener with TLS (rustls / ACM for Nitro Enclaves).
    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, router).await?;

    Ok(())
}
