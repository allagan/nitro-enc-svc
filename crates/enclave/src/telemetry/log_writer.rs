//! Thread-safe TCP writer for forwarding `tracing` log records to the ADOT Collector.
//!
//! [`SharedTcpWriter`] wraps a `TcpStream` behind an `Arc<Mutex<_>>` so that
//! [`tracing_subscriber::fmt::MakeWriter`] can be implemented by cloning the
//! handle. Each clone shares the same underlying connection.
//!
//! The connection target is `127.0.0.1:4318`, which the vsock log bridge in
//! `main.rs` forwards to `vsock(VSOCK_PROXY_CID, 4318)` on the parent EC2.
//! There, a vsock-proxy instance routes the data to the ADOT Collector's
//! `tcplog` receiver (listening on `127.0.0.1:4318`), which exports to
//! CloudWatch Logs.
//!
//! Write errors are silently discarded; the fallback stderr fmt layer always
//! remains active so no log records are permanently lost.

use std::io::{self, Write};
use std::net::TcpStream;
use std::sync::{Arc, Mutex};

/// Shared, thread-safe TCP writer.
///
/// Implements both [`io::Write`] (via the locked inner stream) and
/// [`tracing_subscriber::fmt::MakeWriter`] (by cloning the `Arc`).
#[derive(Clone)]
pub struct SharedTcpWriter(Arc<Mutex<TcpStream>>);

impl SharedTcpWriter {
    /// Connect to `addr` (e.g. `"127.0.0.1:4318"`) and return a writer.
    ///
    /// Returns `None` if the connection cannot be established (e.g. the vsock
    /// bridge port is not yet open). The caller should log a warning and omit
    /// the TCP log layer rather than failing startup.
    pub fn try_connect(addr: &str) -> Option<Self> {
        let stream = TcpStream::connect(addr).ok()?;
        let _ = stream.set_nodelay(true);
        Some(Self(Arc::new(Mutex::new(stream))))
    }
}

impl Write for SharedTcpWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        // Best-effort: discard on lock failure or broken connection.
        match self.0.lock() {
            Ok(mut guard) => guard.write(buf).or(Ok(buf.len())),
            Err(_) => Ok(buf.len()),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        match self.0.lock() {
            Ok(mut guard) => guard.flush().or(Ok(())),
            Err(_) => Ok(()),
        }
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for SharedTcpWriter {
    type Writer = SharedTcpWriter;

    fn make_writer(&'a self) -> SharedTcpWriter {
        // Clone only the Arc, not the underlying TcpStream.
        self.clone()
    }
}
