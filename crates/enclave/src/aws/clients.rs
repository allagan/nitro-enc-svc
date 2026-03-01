//! AWS SDK client bundle initialised for vsock-proxied endpoints.

use anyhow::Result;
use aws_config::BehaviorVersion;

/// Bundle of AWS SDK clients configured to communicate via the vsock proxy.
///
/// All three clients share the same underlying [`aws_config::SdkConfig`] so
/// that credentials are resolved once and reused.
#[derive(Clone)]
pub struct AwsClients {
    /// KMS client used to decrypt the envelope-encrypted DEK.
    pub kms: aws_sdk_kms::Client,
    /// Secrets Manager client used to fetch the encrypted DEK.
    pub secretsmanager: aws_sdk_secretsmanager::Client,
    /// S3 client used to fetch OpenAPI schema files.
    pub s3: aws_sdk_s3::Client,
}

impl AwsClients {
    /// Initialise all AWS SDK clients.
    ///
    /// In production, the SDK is configured to route through the vsock proxy
    /// by overriding the endpoint URL for each service. Credentials are resolved
    /// via the standard AWS credential chain (IAM role attached to the parent
    /// EC2 instance, proxied through the vsock endpoint).
    ///
    /// # Errors
    ///
    /// Returns an error if the SDK config cannot be loaded.
    pub async fn init(_vsock_proxy_cid: u32, vsock_proxy_port: u32) -> Result<Self> {
        // The vsock proxy translates TCP connections from the enclave into vsock
        // connections to the parent EC2, where aws-vsock-proxy forwards them to
        // the real AWS APIs. We point each SDK client at the loopback address of
        // the proxy (the proxy listens on a local TCP port inside the enclave).
        //
        // TODO: replace with actual vsock-aware endpoint resolution when the
        //       enclave networking layer is wired up.
        let endpoint_base = format!("http://127.0.0.1:{vsock_proxy_port}");

        let config = aws_config::defaults(BehaviorVersion::latest()).load().await;

        let kms = aws_sdk_kms::Client::from_conf(
            aws_sdk_kms::config::Builder::from(&config)
                .endpoint_url(format!("{endpoint_base}/kms"))
                .build(),
        );

        let secretsmanager = aws_sdk_secretsmanager::Client::from_conf(
            aws_sdk_secretsmanager::config::Builder::from(&config)
                .endpoint_url(format!("{endpoint_base}/secretsmanager"))
                .build(),
        );

        let s3 = aws_sdk_s3::Client::from_conf(
            aws_sdk_s3::config::Builder::from(&config)
                .endpoint_url(format!("{endpoint_base}/s3"))
                .build(),
        );

        Ok(Self {
            kms,
            secretsmanager,
            s3,
        })
    }
}
