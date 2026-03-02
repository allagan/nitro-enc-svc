//! AWS SDK client bundle initialised for vsock-proxied endpoints.
//!
//! All AWS API calls from inside the enclave route through per-service
//! `vsock-proxy` instances on the parent EC2 host (vsock ports 8001–8003).
//! TLS is handled end-to-end by the enclave: the connector opens a raw vsock
//! stream to the parent proxy, then `hyper-rustls` negotiates TLS directly
//! with the AWS service endpoint.  The parent proxy is a transparent TCP relay.
//!
//! IMDS (for credential/region resolution) uses a socat bridge that the
//! enclave entrypoint starts on 127.0.0.1:8004 → vsock(3,8004).
//! `AWS_EC2_METADATA_SERVICE_ENDPOINT=http://127.0.0.1:8004` (baked into the
//! EIF) redirects the SDK's IMDS client to that bridge.

use std::fmt;

use anyhow::Result;
use aws_config::BehaviorVersion;
use aws_smithy_runtime_api::client::http::{
    HttpClient, HttpConnector, HttpConnectorFuture, HttpConnectorSettings, SharedHttpClient,
    SharedHttpConnector,
};
use aws_smithy_runtime_api::client::orchestrator::HttpResponse;
use aws_smithy_runtime_api::client::result::ConnectorError;
use aws_smithy_runtime_api::client::runtime_components::RuntimeComponents;
use aws_smithy_types::body::SdkBody;
use hyper_rustls::HttpsConnectorBuilder;
use hyper_util::client::legacy::Client;
use hyper_util::rt::TokioExecutor;
use tower::ServiceExt;

use super::vsock_connector::VsockRawConnector;

// ---------------------------------------------------------------------------
// VsockAdapter — HttpConnector backed by a vsock-aware hyper client
// ---------------------------------------------------------------------------

/// Wraps a hyper-util legacy client that routes connections through vsock.
/// Implements the AWS SDK's `HttpConnector` trait so it can be injected into
/// the SDK config.
struct VsockAdapter {
    client: Client<hyper_rustls::HttpsConnector<VsockRawConnector>, SdkBody>,
}

impl fmt::Debug for VsockAdapter {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("VsockAdapter").finish_non_exhaustive()
    }
}

impl HttpConnector for VsockAdapter {
    fn call(
        &self,
        request: aws_smithy_runtime_api::client::orchestrator::HttpRequest,
    ) -> HttpConnectorFuture {
        // Convert the SDK request to an http 1.x request.
        let req = match request.try_into_http1x() {
            Ok(r) => r,
            Err(e) => {
                return HttpConnectorFuture::ready(Err(ConnectorError::user(e.into())));
            }
        };

        // Clone the client (cheap — it's Arc-backed) and drive the request.
        let client = self.client.clone();
        HttpConnectorFuture::new(async move {
            let response = client
                .oneshot(req)
                .await
                .map_err(|e| ConnectorError::io(e.into()))?;

            // Convert hyper's response body to SdkBody, then to HttpResponse.
            let response = response.map(SdkBody::from_body_1_x);
            HttpResponse::try_from(response).map_err(|e| ConnectorError::other(e.into(), None))
        })
    }
}

// ---------------------------------------------------------------------------
// VsockHttpClient — HttpClient that always returns our fixed SharedHttpConnector
// ---------------------------------------------------------------------------

/// Implements the SDK's `HttpClient` factory trait. Since our vsock connector
/// is configuration-independent, we always return the same `SharedHttpConnector`.
struct VsockHttpClient {
    connector: SharedHttpConnector,
}

impl fmt::Debug for VsockHttpClient {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("VsockHttpClient").finish_non_exhaustive()
    }
}

impl HttpClient for VsockHttpClient {
    fn http_connector(
        &self,
        _settings: &HttpConnectorSettings,
        _components: &RuntimeComponents,
    ) -> SharedHttpConnector {
        self.connector.clone()
    }
}

// ---------------------------------------------------------------------------
// AwsClients
// ---------------------------------------------------------------------------

/// Bundle of AWS SDK clients configured to communicate via the vsock proxy.
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
    /// Initialise all AWS SDK clients with a vsock-aware HTTP connector.
    ///
    /// The connector routes HTTPS connections to AWS service endpoints through
    /// vsock to the corresponding `vsock-proxy` on the parent EC2, negotiating
    /// TLS end-to-end with the real AWS endpoint.
    ///
    /// # Errors
    ///
    /// Returns an error if the SDK config cannot be built.
    pub async fn init(vsock_proxy_cid: u32, vsock_proxy_port: u32) -> Result<Self> {
        // Build the vsock raw connector (handles vsock vs. TCP routing).
        let raw = VsockRawConnector::new(vsock_proxy_cid, vsock_proxy_port);

        // Wrap with hyper-rustls to add TLS for HTTPS URIs.
        let https_connector = HttpsConnectorBuilder::new()
            .with_webpki_roots()
            .https_or_http()
            .enable_http1()
            .wrap_connector(raw);

        // Build a hyper legacy HTTP/1 client backed by the vsock+TLS connector.
        let hyper_client = Client::builder(TokioExecutor::new()).build(https_connector);

        // Wrap the hyper client in our SDK HttpConnector + HttpClient adapters.
        let http_client = SharedHttpClient::new(VsockHttpClient {
            connector: SharedHttpConnector::new(VsockAdapter {
                client: hyper_client,
            }),
        });

        // Load SDK config using the custom HTTP client.
        // AWS_REGION and AWS_EC2_METADATA_SERVICE_ENDPOINT are baked into
        // the EIF as env vars; the SDK reads them automatically.
        let config = aws_config::defaults(BehaviorVersion::latest())
            .http_client(http_client)
            .load()
            .await;

        Ok(Self {
            kms: aws_sdk_kms::Client::new(&config),
            secretsmanager: aws_sdk_secretsmanager::Client::new(&config),
            s3: aws_sdk_s3::Client::new(&config),
        })
    }
}
