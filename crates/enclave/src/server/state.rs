//! Shared application state injected into every Axum handler.

use std::sync::Arc;

use crate::dek::DekStore;
use crate::schema::SchemaCache;

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
}

impl AppState {
    /// Create a new [`AppState`] with the provided stores and header name.
    pub fn new(
        dek_store: DekStore,
        schema_cache: SchemaCache,
        schema_header_name: String,
    ) -> Self {
        Self {
            dek_store,
            schema_cache,
            schema_header_name: Arc::new(schema_header_name),
        }
    }
}

impl Default for AppState {
    /// Creates a default [`AppState`] with empty stores, suitable for tests.
    fn default() -> Self {
        Self::new(
            DekStore::new(),
            SchemaCache::new(),
            "X-Schema-Name".into(),
        )
    }
}
