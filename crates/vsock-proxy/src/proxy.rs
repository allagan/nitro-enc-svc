//! Bidirectional TCP ↔ vsock forwarding.
//!
//! For each incoming TCP connection the proxy:
//! 1. Opens a new vsock stream to the enclave.
//! 2. Spawns two Tokio tasks: one copying bytes TCP→vsock, the other vsock→TCP.
//! 3. When either half closes, both tasks shut down.
//!
//! TLS bytes are forwarded **opaquely** — TLS terminates inside the enclave,
//! not in this sidecar. The sidecar has no visibility into plaintext.

use anyhow::Result;
use std::net::SocketAddr;
use tokio::{
    io::{self, AsyncRead, AsyncWrite},
    net::{TcpListener, TcpStream},
};
use tokio_vsock::{VsockAddr, VsockStream};
use tracing::{debug, error, info, warn};

use crate::config::Config;

/// Accept loop: listen on TCP and proxy each connection to the enclave vsock port.
///
/// Runs until the process is killed.
///
/// # Errors
///
/// Returns an error if the TCP listener cannot be bound.
pub async fn run(cfg: &Config) -> Result<()> {
    let addr: SocketAddr = ([0u8, 0, 0, 0], cfg.listen_port).into();
    let listener = TcpListener::bind(addr).await?;
    info!(addr = %addr, enclave_cid = cfg.enclave_cid, enclave_port = cfg.enclave_port, "vsock-proxy listening");

    loop {
        match listener.accept().await {
            Ok((tcp_stream, peer_addr)) => {
                debug!(%peer_addr, "accepted TCP connection");
                let cid = cfg.enclave_cid;
                let port = cfg.enclave_port;
                tokio::spawn(async move {
                    if let Err(e) = handle_connection(tcp_stream, cid, port).await {
                        warn!(%peer_addr, error = %e, "connection error");
                    }
                });
            }
            Err(e) => {
                error!(error = %e, "accept error");
            }
        }
    }
}

/// Handle a single TCP ↔ vsock connection.
async fn handle_connection(tcp: TcpStream, enclave_cid: u32, enclave_port: u32) -> Result<()> {
    let vsock = VsockStream::connect(VsockAddr::new(enclave_cid, enclave_port)).await?;
    debug!(enclave_cid, enclave_port, "vsock connection established");

    let (tcp_read, tcp_write) = io::split(tcp);
    let (vsock_read, vsock_write) = io::split(vsock);

    let tcp_to_vsock = copy_half(tcp_read, vsock_write, "tcp→vsock");
    let vsock_to_tcp = copy_half(vsock_read, tcp_write, "vsock→tcp");

    // Run both directions concurrently; stop when either half finishes.
    tokio::select! {
        res = tcp_to_vsock => { res? }
        res = vsock_to_tcp => { res? }
    }

    Ok(())
}

/// Copy bytes from `reader` to `writer`, logging the direction on completion.
async fn copy_half<R, W>(mut reader: R, mut writer: W, label: &'static str) -> Result<()>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let bytes = io::copy(&mut reader, &mut writer).await?;
    debug!(label, bytes, "half-close");
    Ok(())
}

#[cfg(test)]
mod tests {
    /// The forwarding logic is exercised by integration tests that spin up a
    /// mock enclave. Unit tests here cover configuration-level checks only.
    #[test]
    fn placeholder() {
        // Real proxy tests live in tests/ and require a vsock-capable host.
    }
}
