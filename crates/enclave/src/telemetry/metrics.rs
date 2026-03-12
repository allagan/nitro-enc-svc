//! Application-level OTEL metric instruments.
//!
//! [`Metrics`] is constructed once at startup from the global meter provider
//! (which must have been initialised by [`super::init_telemetry`] first) and
//! then stored in [`crate::server::state::AppState`] for use by handlers and
//! background tasks.
//!
//! # Invariant
//!
//! No PII or key material must appear in any metric label or attribute.

use opentelemetry::{
    metrics::{Counter, Histogram, Meter, Unit},
    KeyValue,
};

/// OTEL metric instruments recorded by the enclave service.
pub struct Metrics {
    /// Count of `/encrypt` requests. Label: `status` = `"success"` | `"error"`.
    pub encrypt_requests: Counter<u64>,
    /// Latency of `/encrypt` requests in milliseconds. Label: `status` = `"success"` | `"error"`.
    pub encrypt_latency_ms: Histogram<f64>,
    /// Count of `/decrypt` requests. Label: `status` = `"success"` | `"error"`.
    pub decrypt_requests: Counter<u64>,
    /// Latency of `/decrypt` requests in milliseconds. Label: `status` = `"success"` | `"error"`.
    pub decrypt_latency_ms: Histogram<f64>,
    /// Count of successful DEK rotations (background task).
    pub dek_rotations: Counter<u64>,
}

impl Metrics {
    /// Construct all instruments from `meter`.
    ///
    /// Must be called after [`super::init_telemetry`] so that the global
    /// meter provider is set.
    pub fn new(meter: &Meter) -> Self {
        Self {
            encrypt_requests: meter
                .u64_counter("enclave_encrypt_requests")
                .with_description("Total number of /encrypt requests")
                .init(),
            encrypt_latency_ms: meter
                .f64_histogram("enclave_encrypt_latency_ms")
                .with_description("Latency of /encrypt requests in milliseconds")
                .with_unit(Unit::new("ms"))
                .init(),
            decrypt_requests: meter
                .u64_counter("enclave_decrypt_requests")
                .with_description("Total number of /decrypt requests")
                .init(),
            decrypt_latency_ms: meter
                .f64_histogram("enclave_decrypt_latency_ms")
                .with_description("Latency of /decrypt requests in milliseconds")
                .with_unit(Unit::new("ms"))
                .init(),
            dek_rotations: meter
                .u64_counter("enclave_dek_rotations")
                .with_description("Number of successful DEK background rotations")
                .init(),
        }
    }

    /// Convenience: attribute slice for a success outcome.
    #[inline]
    pub fn success_attrs() -> [KeyValue; 1] {
        [KeyValue::new("status", "success")]
    }

    /// Convenience: attribute slice for an error outcome.
    #[inline]
    pub fn error_attrs() -> [KeyValue; 1] {
        [KeyValue::new("status", "error")]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use opentelemetry::global;

    #[test]
    fn metrics_new_does_not_panic() {
        // The global meter provider defaults to a no-op provider when unset,
        // so constructing Metrics without calling init_telemetry is safe in tests.
        let meter = global::meter("test");
        let _m = Metrics::new(&meter);
    }
}
