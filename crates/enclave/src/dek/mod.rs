//! DEK (Data Encryption Key) fetch, decrypt, cache, and background rotation.
//!
//! # Lifecycle
//!
//! 1. At startup, [`fetch_and_store`] fetches the envelope-encrypted DEK from
//!    AWS Secrets Manager and decrypts it via AWS KMS.
//! 2. The decrypted DEK lives only in enclave memory, wrapped in an `Arc<RwLock<_>>`.
//! 3. A background Tokio task calls [`rotation_task`] on a configurable interval
//!    to refresh the cached DEK.
//! 4. Encryption handlers borrow the DEK via [`DekStore::current`], which acquires a
//!    short read lock and clones the key bytes into a zeroizable buffer.
//!
//! # Security invariants
//!
//! - The plaintext DEK is **never** written to disk, logged, or included in traces.
//! - KMS key policy enforces Nitro attestation (PCR values); decryption fails if the
//!   enclave image does not match the expected measurements.

pub mod store;

pub use store::DekStore;

use anyhow::{Context, Result};
use tokio::time;
use tracing::{info, warn};

use crate::aws::AwsClients;
use crate::config::Config;

/// Fetch the envelope-encrypted DEK from Secrets Manager, decrypt it via KMS,
/// and store the plaintext key bytes in `store`.
///
/// # Errors
///
/// Returns an error if the Secrets Manager call fails, if KMS decryption fails,
/// or if the decrypted key material is not exactly 32 bytes.
pub async fn fetch_and_store(aws: &AwsClients, cfg: &Config, store: &DekStore) -> Result<()> {
    // Fetch the envelope-encrypted DEK blob from Secrets Manager.
    let secret = aws
        .secretsmanager
        .get_secret_value()
        .secret_id(&cfg.secret_arn)
        .send()
        .await
        .context("failed to fetch DEK from Secrets Manager")?;

    // The DEK ciphertext is expected to be stored as binary.
    let ciphertext_bytes = secret
        .secret_binary()
        .context("DEK secret must be stored as binary in Secrets Manager")?
        .as_ref()
        .to_vec();

    // Decrypt the ciphertext blob via KMS.
    let decrypt_resp = aws
        .kms
        .decrypt()
        .key_id(&cfg.kms_key_id)
        .ciphertext_blob(aws_sdk_kms::primitives::Blob::new(ciphertext_bytes))
        .send()
        .await
        .context("failed to decrypt DEK via KMS")?;

    let plaintext = decrypt_resp
        .plaintext()
        .context("KMS decrypt response contained no plaintext")?;

    store
        .store(plaintext.as_ref())
        .await
        .context("failed to store decrypted DEK (unexpected key length)")?;

    info!("DEK fetched and stored successfully");
    Ok(())
}

/// Spawn a background task that periodically re-fetches and rotates the DEK.
///
/// The first rotation fires after one full interval (startup fetch is assumed
/// to have already populated the store). On rotation failure the previous key
/// is retained and a warning is emitted.
pub fn rotation_task(aws: AwsClients, cfg: Config, store: DekStore) -> tokio::task::JoinHandle<()> {
    let interval = std::time::Duration::from_secs(cfg.dek_rotation_interval_secs);
    tokio::spawn(async move {
        let mut ticker = time::interval(interval);
        // First tick fires immediately — skip it so we don't double-fetch.
        ticker.tick().await;
        loop {
            ticker.tick().await;
            match fetch_and_store(&aws, &cfg, &store).await {
                Ok(()) => info!("DEK rotated successfully"),
                Err(e) => warn!(error = %e, "DEK rotation failed; retaining previous key"),
            }
        }
    })
}
