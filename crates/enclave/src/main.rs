//! `nitro-enc-svc` — enclave binary entry point.
//!
//! Startup sequence:
//! 1. Load and validate [`Config`] from environment variables.
//! 2. Start the IMDS vsock bridge (TCP 127.0.0.1:8004 → vsock(parent,8004)).
//! 3. Initialise the telemetry pipeline (OTEL + tracing).
//! 4. Initialise AWS SDK clients pointing at the vsock proxy.
//! 5. Fetch + decrypt the DEK from Secrets Manager / KMS and seed [`DekStore`].
//! 6. Load OpenAPI schemas from S3 into [`SchemaCache`].
//! 7. Spawn background tasks: DEK rotation, schema refresh.
//! 8. Build the Axum router and start the TLS server.

mod aws;
mod config;
mod crypto;
mod dek;
mod schema;
mod server;
mod telemetry;

use anyhow::{Context, Result};
use hyper_util::rt::{TokioExecutor, TokioIo};
use tokio::net::TcpListener;
use tokio_rustls::TlsAcceptor;
use tokio_vsock::{VsockAddr, VsockStream};
use tower::ServiceExt as _;
use tracing::{error, info, warn};

use config::Config;
use dek::DekStore;
use schema::SchemaCache;
use server::state::AppState;

/// Spawn a background task that bridges TCP 127.0.0.1:8004 → vsock(parent_cid, 8004).
///
/// The AWS SDK inside the enclave sends IMDS requests to
/// `AWS_EC2_METADATA_SERVICE_ENDPOINT=http://127.0.0.1:8004`. This bridge
/// receives those TCP connections and forwards them over vsock to the
/// `vsock-proxy` running on the parent EC2, which relays them to
/// 169.254.169.254:80 (the real IMDS endpoint).
///
/// The bridge is implemented in Rust (tokio-vsock) to avoid depending on
/// socat being compiled with VSOCK support in the enclave OS image.
async fn start_imds_bridge(parent_cid: u32) -> Result<()> {
    const IMDS_LOCAL_PORT: u16 = 8004;
    const IMDS_VSOCK_PORT: u32 = 8004;

    let listener = TcpListener::bind(("127.0.0.1", IMDS_LOCAL_PORT))
        .await
        .context("failed to bind IMDS bridge on 127.0.0.1:8004")?;

    eprintln!(
        "INFO: IMDS bridge listening on 127.0.0.1:{IMDS_LOCAL_PORT} \
         -> vsock({parent_cid},{IMDS_VSOCK_PORT})"
    );

    tokio::spawn(async move {
        loop {
            let (tcp, _peer) = match listener.accept().await {
                Ok(c) => c,
                Err(e) => {
                    eprintln!("WARN: IMDS bridge accept error: {e}");
                    continue;
                }
            };
            tokio::spawn(async move {
                let vsock =
                    match VsockStream::connect(VsockAddr::new(parent_cid, IMDS_VSOCK_PORT)).await {
                        Ok(s) => s,
                        Err(e) => {
                            eprintln!("WARN: IMDS bridge vsock connect error: {e}");
                            return;
                        }
                    };
                let (mut tr, mut tw) = tokio::io::split(tcp);
                let (mut vr, mut vw) = tokio::io::split(vsock);
                tokio::select! {
                    _ = tokio::io::copy(&mut tr, &mut vw) => {}
                    _ = tokio::io::copy(&mut vr, &mut tw) => {}
                }
            });
        }
    });

    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    // Install the aws-lc-rs Rustls CryptoProvider as the process default.
    // Both hyper-rustls and opentelemetry-otlp (via tonic) pull in rustls
    // 0.23.x which requires an explicit default when multiple provider features
    // (ring + aws-lc-rs) are compiled in from different transitive deps.
    rustls::crypto::aws_lc_rs::default_provider()
        .install_default()
        .expect("failed to install rustls CryptoProvider");

    // -----------------------------------------------------------------------
    // 1. Configuration
    // -----------------------------------------------------------------------
    let cfg = Config::from_env().map_err(|e| {
        // Telemetry is not yet up; write to stderr directly.
        eprintln!("ERROR: configuration invalid: {e}");
        e
    })?;

    // -----------------------------------------------------------------------
    // 2. IMDS vsock bridge
    // -----------------------------------------------------------------------
    // Must start before AwsClients::init() so the SDK's credential resolver
    // can reach IMDS via http://127.0.0.1:8004.
    start_imds_bridge(cfg.vsock_proxy_cid).await?;

    // -----------------------------------------------------------------------
    // 3. Telemetry
    // -----------------------------------------------------------------------
    telemetry::init_telemetry(&cfg.otel_exporter_otlp_endpoint, &cfg.log_level)?;
    info!(
        version = env!("CARGO_PKG_VERSION"),
        tls_port = cfg.tls_port,
        "nitro-enc-svc starting"
    );

    // -----------------------------------------------------------------------
    // 4. AWS clients
    // -----------------------------------------------------------------------
    let aws = aws::AwsClients::init(cfg.vsock_proxy_cid, cfg.vsock_proxy_port).await?;

    // -----------------------------------------------------------------------
    // 5. DEK initialisation
    // -----------------------------------------------------------------------
    let dek_store = DekStore::new();
    dek::fetch_and_store(&aws, &cfg, &dek_store).await?;

    // -----------------------------------------------------------------------
    // 6. Schema cache initialisation
    // -----------------------------------------------------------------------
    let schema_cache = SchemaCache::new();
    schema::load_all(&aws, &cfg, &schema_cache).await?;

    // -----------------------------------------------------------------------
    // 7. Background tasks
    // -----------------------------------------------------------------------
    let _dek_rotation = dek::rotation_task(aws.clone(), cfg.clone(), dek_store.clone());
    let _schema_refresh = schema::refresh_task(aws.clone(), cfg.clone(), schema_cache.clone());

    // -----------------------------------------------------------------------
    // 8. TLS configuration (cert + key written by ACM for Nitro Enclaves)
    // -----------------------------------------------------------------------
    let cert_pem = std::fs::read(&cfg.tls_cert_path)
        .with_context(|| format!("failed to read TLS cert: {}", cfg.tls_cert_path))?;
    let key_pem = std::fs::read(&cfg.tls_key_path)
        .with_context(|| format!("failed to read TLS key: {}", cfg.tls_key_path))?;
    let tls_cfg = server::tls::build_server_config(&cert_pem, &key_pem)?;
    let tls_acceptor = TlsAcceptor::from(tls_cfg);

    // -----------------------------------------------------------------------
    // 9. HTTPS server (TLS accept loop)
    // -----------------------------------------------------------------------
    let state = AppState::new(dek_store, schema_cache, cfg.schema_header_name.clone());
    let router = server::router::build(state);

    let addr: std::net::SocketAddr = ([0, 0, 0, 0], cfg.tls_port).into();
    info!(addr = %addr, "listening (TLS)");
    let listener = tokio::net::TcpListener::bind(addr).await?;

    loop {
        let (tcp_stream, peer_addr) = listener.accept().await?;
        let acceptor = tls_acceptor.clone();
        let router = router.clone();

        tokio::spawn(async move {
            let tls_stream = match acceptor.accept(tcp_stream).await {
                Ok(s) => s,
                Err(e) => {
                    warn!(peer = %peer_addr, err = %e, "TLS handshake failed");
                    return;
                }
            };

            let io = TokioIo::new(tls_stream);
            let svc =
                hyper::service::service_fn(move |req: hyper::Request<hyper::body::Incoming>| {
                    router.clone().oneshot(req.map(axum::body::Body::new))
                });

            if let Err(e) = hyper_util::server::conn::auto::Builder::new(TokioExecutor::new())
                .serve_connection(io, svc)
                .await
            {
                error!(peer = %peer_addr, err = %e, "connection error");
            }
        });
    }
}
