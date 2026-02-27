//! OpenTelemetry setup: metrics, traces, and structured logs exported via vsock.
//!
//! The OTEL SDK inside the enclave exports via OTLP/gRPC through vsock to the
//! OTEL Collector running on the parent EC2 instance.
//!
//! # Telemetry invariants
//!
//! - **No PII or key material** must appear in any span attribute, metric label,
//!   or log field.
//! - Log level is configurable via `LOG_LEVEL` (default: `info`).

pub mod init;

pub use init::init_telemetry;
