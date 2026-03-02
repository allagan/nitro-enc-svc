//! Custom hyper connector that routes AWS API calls through vsock to the
//! `vsock-proxy` instances running on the parent EC2 host.
//!
//! # Architecture
//!
//! ```text
//! AWS SDK (enclave)
//!   └─ VsockRawConnector  ──vsock(CID=3, port N)──► vsock-proxy (parent EC2)
//!                                                        └─ TCP/TLS ──► AWS endpoint
//! ```
//!
//! Port mapping (base = VSOCK_PROXY_PORT, default 8000):
//! - base+1 (8001): KMS
//! - base+2 (8002): Secrets Manager
//! - base+3 (8003): S3
//!
//! IMDS (for credential resolution) is not handled here. The enclave
//! entrypoint starts a socat bridge on 127.0.0.1:8004 → vsock(3, 8004),
//! and `AWS_EC2_METADATA_SERVICE_ENDPOINT=http://127.0.0.1:8004` redirects
//! IMDS traffic to that bridge via plain TCP (handled by the default SDK
//! HTTP stack, bypassing this connector).

use std::future::Future;
use std::io;
use std::pin::Pin;
use std::task::{Context, Poll};

use anyhow::{Context as _, Result};
use hyper::rt::{ReadBufCursor, Write};
use hyper::Uri;
use hyper_util::client::legacy::connect::{Connected, Connection};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::net::TcpStream;
use tokio_vsock::{VsockAddr, VsockStream};
use tower::Service;

/// Maps an AWS service hostname to a vsock port number on the parent EC2.
fn vsock_port(host: &str, base_port: u32) -> u32 {
    if host.starts_with("kms.") {
        base_port + 1
    } else if host.starts_with("secretsmanager.") {
        base_port + 2
    } else if host.starts_with("s3.") || host == "s3.amazonaws.com" {
        base_port + 3
    } else {
        // Unknown service — fall back to KMS port so the error is visible.
        base_port + 1
    }
}

// ---------------------------------------------------------------------------
// Raw stream type returned by VsockRawConnector
// ---------------------------------------------------------------------------

/// Either a vsock stream (to the parent EC2 proxy) or a plain TCP stream
/// (for local IMDS-redirect connections at 127.0.0.1:8004).
///
/// Implements hyper 1.x's `rt::Read + rt::Write` so that `hyper-rustls`'s
/// `HttpsConnector` can wrap it with TLS.
pub enum RawStream {
    Vsock(VsockStream),
    Tcp(TcpStream),
}

impl Connection for RawStream {
    fn connected(&self) -> Connected {
        Connected::new()
    }
}

/// Bridge from tokio's `AsyncRead` to hyper's `rt::Read`.
///
/// Uses the same approach as `hyper_util::rt::TokioIo`: creates a temporary
/// `tokio::io::ReadBuf` backed by the cursor's uninit slice, delegates to
/// the underlying tokio stream, and advances the cursor by the filled count.
impl hyper::rt::Read for RawStream {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        mut buf: ReadBufCursor<'_>,
    ) -> Poll<io::Result<()>> {
        // SAFETY: we pass buf.as_mut() (the uninit region) directly to
        // tokio's ReadBuf, which tracks initialization correctly. We then
        // advance the cursor by exactly the number of bytes filled.
        let n = unsafe {
            let mut tbuf = ReadBuf::uninit(buf.as_mut());
            let poll = match self.get_mut() {
                RawStream::Vsock(s) => Pin::new(s).poll_read(cx, &mut tbuf),
                RawStream::Tcp(s) => Pin::new(s).poll_read(cx, &mut tbuf),
            };
            match poll {
                Poll::Ready(Ok(())) => tbuf.filled().len(),
                other => return other,
            }
        };
        unsafe { buf.advance(n) };
        Poll::Ready(Ok(()))
    }
}

impl Write for RawStream {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        match self.get_mut() {
            RawStream::Vsock(s) => Pin::new(s).poll_write(cx, buf),
            RawStream::Tcp(s) => Pin::new(s).poll_write(cx, buf),
        }
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        match self.get_mut() {
            RawStream::Vsock(s) => Pin::new(s).poll_flush(cx),
            RawStream::Tcp(s) => Pin::new(s).poll_flush(cx),
        }
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        match self.get_mut() {
            RawStream::Vsock(s) => Pin::new(s).poll_shutdown(cx),
            RawStream::Tcp(s) => Pin::new(s).poll_shutdown(cx),
        }
    }
}

impl Unpin for RawStream {}

// ---------------------------------------------------------------------------
// Connector
// ---------------------------------------------------------------------------

/// Hyper connector that opens vsock connections to the parent EC2 host for
/// AWS service endpoints, and plain TCP connections for localhost (IMDS).
#[derive(Clone)]
pub struct VsockRawConnector {
    /// Vsock CID of the parent EC2 host (typically 3).
    cid: u32,
    /// Base vsock port. KMS = base+1, SM = base+2, S3 = base+3.
    base_port: u32,
}

impl VsockRawConnector {
    /// Create a new connector.
    pub fn new(cid: u32, base_port: u32) -> Self {
        Self { cid, base_port }
    }
}

impl Service<Uri> for VsockRawConnector {
    type Response = RawStream;
    type Error = Box<dyn std::error::Error + Send + Sync>;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, uri: Uri) -> Self::Future {
        let cid = self.cid;
        let base_port = self.base_port;

        Box::pin(async move {
            let host = uri.host().unwrap_or("").to_owned();

            // Local addresses (e.g. IMDS redirect on 127.0.0.1:8004): plain TCP.
            if host == "127.0.0.1" || host == "localhost" {
                let port = uri.port_u16().unwrap_or(80);
                let stream = TcpStream::connect((host.as_str(), port))
                    .await
                    .with_context(|| format!("TCP connect to {host}:{port}"))?;
                return Ok(RawStream::Tcp(stream));
            }

            // AWS service endpoint: open vsock to the parent proxy.
            let port = vsock_port(&host, base_port);
            let addr = VsockAddr::new(cid, port);
            let stream = VsockStream::connect(addr)
                .await
                .with_context(|| format!("vsock connect to CID={cid} port={port} for {host}"))?;
            Ok(RawStream::Vsock(stream))
        })
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn port_mapping_kms() {
        assert_eq!(vsock_port("kms.us-east-2.amazonaws.com", 8000), 8001);
    }

    #[test]
    fn port_mapping_secretsmanager() {
        assert_eq!(
            vsock_port("secretsmanager.us-east-2.amazonaws.com", 8000),
            8002
        );
    }

    #[test]
    fn port_mapping_s3() {
        assert_eq!(vsock_port("s3.us-east-2.amazonaws.com", 8000), 8003);
        assert_eq!(vsock_port("s3.amazonaws.com", 8000), 8003);
    }
}
