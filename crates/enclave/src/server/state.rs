//! Shared application state injected into every Axum handler.

use std::sync::Arc;

use crate::dek::DekStore;
use crate::schema::SchemaCache;
use crate::telemetry::Metrics;

/// Application state shared across all request handlers.
///
/// All fields are cheaply cloneable (`Arc`-wrapped or already `Arc`-backed) so
/// that Axum can clone the state for each request without copying expensive data.
#[derive(Clone)]
pub struct AppState {
    /// Thread-safe store for the current Data Encryption Key.
    pub dek_store: DekStore,
    /// Lock-free cache of parsed OpenAPI schemas.
    pub schema_cache: SchemaCache,
    /// Name of the HTTP header used to identify the schema for each request.
    pub schema_header_name: Arc<String>,
    /// OTEL metric instruments recorded by request handlers.
    pub metrics: Arc<Metrics>,
}

impl AppState {
    /// Create a new [`AppState`] with the provided stores, header name, and metrics.
    pub fn new(
        dek_store: DekStore,
        schema_cache: SchemaCache,
        schema_header_name: String,
        metrics: Arc<Metrics>,
    ) -> Self {
        Self {
            dek_store,
            schema_cache,
            schema_header_name: Arc::new(schema_header_name),
            metrics,
        }
    }
}

impl Default for AppState {
    /// Creates a default [`AppState`] with empty stores, suitable for tests.
    fn default() -> Self {
        let meter = opentelemetry::global::meter("nitro-enc-svc");
        Self::new(
            DekStore::new(),
            SchemaCache::new(),
            "X-Schema-Name".into(),
            Arc::new(Metrics::new(&meter)),
        )
    }
}
