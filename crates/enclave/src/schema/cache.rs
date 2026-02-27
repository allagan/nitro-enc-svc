//! In-memory cache of parsed OpenAPI schemas, keyed by schema name.
//!
//! Schemas are loaded at startup and refreshed on a configurable interval.
//! The cache uses `arc-swap` for lock-free reads on the hot path.

use std::{collections::HashMap, sync::Arc};

use arc_swap::ArcSwap;
use openapiv3::OpenAPI;
use thiserror::Error;

use super::resolver::{resolve_pii_paths, PiiFieldPaths};

/// Errors from the schema cache.
#[derive(Debug, Error)]
pub enum CacheError {
    /// The requested schema name has no entry in the cache.
    #[error("unknown schema: {0}")]
    UnknownSchema(String),
}

/// A single cached entry: the parsed API document and its derived PII paths.
#[derive(Debug, Clone)]
pub struct CachedSchema {
    /// The parsed OpenAPI document. Retained for future validation / debugging.
    #[allow(dead_code)]
    pub api: Arc<OpenAPI>,
    /// Pre-computed set of dot-notation paths that are marked `x-pii: true`.
    pub pii_paths: Arc<PiiFieldPaths>,
}

/// Shared, lock-free cache of schemas keyed by schema name.
///
/// Internally backed by [`ArcSwap`] so readers never block and the background
/// refresh task can atomically swap in a completely new map.
#[derive(Clone, Debug)]
pub struct SchemaCache {
    inner: Arc<ArcSwap<HashMap<String, CachedSchema>>>,
}

impl SchemaCache {
    /// Create a new, empty [`SchemaCache`].
    pub fn new() -> Self {
        Self {
            inner: Arc::new(ArcSwap::new(Arc::new(HashMap::new()))),
        }
    }

    /// Return the number of schemas currently cached.
    pub fn len(&self) -> usize {
        self.inner.load().len()
    }

    /// Return `true` if no schemas are cached.
    #[allow(dead_code)]
    pub fn is_empty(&self) -> bool {
        self.inner.load().is_empty()
    }

    /// Look up a schema by name.
    ///
    /// This is a lock-free read; safe to call on the hot encryption path.
    ///
    /// # Errors
    ///
    /// Returns [`CacheError::UnknownSchema`] if `name` is not present.
    pub fn get(&self, name: &str) -> Result<CachedSchema, CacheError> {
        self.inner
            .load()
            .get(name)
            .cloned()
            .ok_or_else(|| CacheError::UnknownSchema(name.to_owned()))
    }

    /// Atomically replace the entire schema map.
    ///
    /// Called by the background refresh task after fetching and parsing all
    /// schema files from S3.
    pub fn replace_all(&self, schemas: HashMap<String, OpenAPI>) {
        let new_map: HashMap<String, CachedSchema> = schemas
            .into_iter()
            .map(|(name, api)| {
                let pii_paths = resolve_pii_paths(&api);
                let entry = CachedSchema {
                    api: Arc::new(api),
                    pii_paths: Arc::new(pii_paths),
                };
                (name, entry)
            })
            .collect();
        self.inner.store(Arc::new(new_map));
    }
}

impl Default for SchemaCache {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_empty_api() -> OpenAPI {
        serde_json::from_str(
            r#"{"openapi":"3.0.0","info":{"title":"t","version":"1"},"paths":{}}"#,
        )
        .unwrap()
    }

    #[test]
    fn initially_empty() {
        let cache = SchemaCache::new();
        assert!(cache.is_empty());
        assert_eq!(cache.len(), 0);
    }

    #[test]
    fn unknown_schema_returns_error() {
        let cache = SchemaCache::new();
        assert!(cache.get("nonexistent").is_err());
    }

    #[test]
    fn replace_all_and_get() {
        let cache = SchemaCache::new();
        let mut map = HashMap::new();
        map.insert("payments-v1".into(), make_empty_api());
        cache.replace_all(map);
        assert_eq!(cache.len(), 1);
        assert!(cache.get("payments-v1").is_ok());
        assert!(cache.get("other").is_err());
    }

    #[test]
    fn replace_all_is_atomic() {
        let cache = SchemaCache::new();
        let mut map1 = HashMap::new();
        map1.insert("schema-a".into(), make_empty_api());
        cache.replace_all(map1);

        let mut map2 = HashMap::new();
        map2.insert("schema-b".into(), make_empty_api());
        cache.replace_all(map2);

        // Only schema-b should be present after the second replace.
        assert!(cache.get("schema-a").is_err());
        assert!(cache.get("schema-b").is_ok());
    }
}
