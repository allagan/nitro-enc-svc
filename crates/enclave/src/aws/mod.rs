//! AWS SDK client initialisation for KMS, Secrets Manager, and S3.
//!
//! All AWS API calls from inside the enclave must be routed through the
//! `aws-vsock-proxy` running on the parent EC2 instance. This module
//! configures each SDK client to target the correct vsock endpoint.

pub mod clients;

pub use clients::AwsClients;
