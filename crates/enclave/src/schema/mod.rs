//! OpenAPI spec loading from S3, PII field resolution, and schema caching.
//!
//! # Responsibilities
//!
//! - Fetch OpenAPI spec files from S3 at startup and on a refresh interval.
//! - Parse specs and index all properties annotated with `x-pii: true`.
//! - Given a schema name and a JSON value, return the set of dot-notation field
//!   paths that must be encrypted (e.g. `"user.address.ssn"`, `"orders[].card_number"`).
//!
//! # Module invariants
//!
//! - **No crypto dependencies.** This module must not import anything from `crate::crypto`
//!   or `crate::dek`.
//! - **No AWS KMS dependency.** S3 reads are allowed; KMS is not.

pub mod cache;
pub mod resolver;

pub use cache::SchemaCache;
pub use resolver::PiiFieldPaths;

use std::collections::HashMap;

use anyhow::{Context, Result};
use openapiv3::OpenAPI;
use tokio::time;
use tracing::{info, warn};

use crate::aws::AwsClients;
use crate::config::Config;

/// Fetch all OpenAPI schema files from S3 and atomically replace the cache.
///
/// Lists objects under `cfg.s3_prefix`, fetches each one, parses it as YAML
/// (falling back to JSON), extracts PII field paths, and calls
/// [`SchemaCache::replace_all`].
///
/// # Errors
///
/// Returns an error if the S3 list call fails or if any individual object
/// cannot be fetched or parsed.
pub async fn load_all(aws: &AwsClients, cfg: &Config, cache: &SchemaCache) -> Result<()> {
    let list = aws
        .s3
        .list_objects_v2()
        .bucket(&cfg.s3_bucket)
        .prefix(&cfg.s3_prefix)
        .send()
        .await
        .context("failed to list S3 objects for schemas")?;

    let objects = list.contents().to_vec();
    if objects.is_empty() {
        warn!(
            bucket = %cfg.s3_bucket,
            prefix = %cfg.s3_prefix,
            "no schema files found in S3"
        );
    }

    let mut schemas: HashMap<String, OpenAPI> = HashMap::new();

    for obj in &objects {
        let key = match obj.key() {
            Some(k) => k,
            None => continue,
        };

        let name = schema_name_from_key(key, &cfg.s3_prefix);

        let get = aws
            .s3
            .get_object()
            .bucket(&cfg.s3_bucket)
            .key(key)
            .send()
            .await
            .with_context(|| format!("failed to fetch schema from S3: {key}"))?;

        let body_bytes = get
            .body
            .collect()
            .await
            .with_context(|| format!("failed to read body for S3 key: {key}"))?
            .into_bytes();

        let text = std::str::from_utf8(&body_bytes)
            .with_context(|| format!("S3 object {key} is not valid UTF-8"))?;

        let api: OpenAPI = if let Ok(parsed) = serde_yaml::from_str(text) {
            parsed
        } else if let Ok(parsed) = serde_json::from_str(text) {
            parsed
        } else {
            anyhow::bail!(
                "failed to parse OpenAPI schema from S3 key {key}: not valid YAML or JSON"
            );
        };

        info!(schema = %name, key = %key, "loaded schema from S3");
        schemas.insert(name, api);
    }

    cache.replace_all(schemas);
    info!(count = cache.len(), "schema cache refreshed");
    Ok(())
}

/// Spawn a background task that periodically refreshes the schema cache from S3.
///
/// On refresh failure the previous cache contents are retained and a warning is
/// emitted; the service continues to operate with stale schemas.
pub fn refresh_task(
    aws: AwsClients,
    cfg: Config,
    cache: SchemaCache,
) -> tokio::task::JoinHandle<()> {
    let interval = std::time::Duration::from_secs(cfg.schema_refresh_interval_secs);
    tokio::spawn(async move {
        let mut ticker = time::interval(interval);
        // First tick fires immediately — skip it so we don't double-load at startup.
        ticker.tick().await;
        loop {
            ticker.tick().await;
            match load_all(&aws, &cfg, &cache).await {
                Ok(()) => info!("schema cache refreshed"),
                Err(e) => warn!(error = %e, "schema refresh failed; retaining previous cache"),
            }
        }
    })
}

/// Derive a schema name from an S3 object key.
///
/// Strips the configured prefix and any file extension (`.yaml`, `.yml`, `.json`).
fn schema_name_from_key(key: &str, prefix: &str) -> String {
    let without_prefix = key.strip_prefix(prefix).unwrap_or(key);
    for ext in [".yaml", ".yml", ".json"] {
        if let Some(stem) = without_prefix.strip_suffix(ext) {
            return stem.to_owned();
        }
    }
    without_prefix.to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schema_name_strips_prefix_and_extension() {
        assert_eq!(schema_name_from_key("schemas/payments-v1.yaml", "schemas/"), "payments-v1");
        assert_eq!(schema_name_from_key("schemas/users.json", "schemas/"), "users");
        assert_eq!(schema_name_from_key("schemas/orders.yml", "schemas/"), "orders");
    }

    #[test]
    fn schema_name_no_prefix_match() {
        assert_eq!(schema_name_from_key("other/file.yaml", "schemas/"), "other/file");
    }

    #[test]
    fn schema_name_no_extension() {
        assert_eq!(schema_name_from_key("schemas/bare", "schemas/"), "bare");
    }
}
