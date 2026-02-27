//! Request and response types exchanged between components.
//!
//! These types are serialised as JSON over both the public HTTPS API and any
//! internal vsock channels.

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Encrypt endpoint
// ---------------------------------------------------------------------------

/// Request body for `POST /encrypt`.
///
/// The `payload` field contains an arbitrary JSON object whose PII fields will
/// be identified via the OpenAPI schema named in the `X-Schema-Name` request
/// header and replaced with encrypted ciphertext strings.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EncryptRequest {
    /// Arbitrary JSON object to encrypt PII fields within.
    pub payload: serde_json::Value,
}

/// Successful response body for `POST /encrypt`.
///
/// The `payload` field mirrors the input structure with PII fields replaced by
/// `v1.<nonce>.<ciphertext>` strings.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EncryptResponse {
    /// Transformed JSON object with PII fields encrypted.
    pub payload: serde_json::Value,
}

// ---------------------------------------------------------------------------
// Error response
// ---------------------------------------------------------------------------

/// Standard error response body returned on any non-2xx status.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorResponse {
    /// Short machine-readable error code (e.g. `"bad_request"`).
    pub code: String,
    /// Human-readable description safe to expose to callers.
    pub message: String,
}

impl ErrorResponse {
    /// Construct an [`ErrorResponse`] from a code and message.
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
        }
    }
}

// ---------------------------------------------------------------------------
// Health check
// ---------------------------------------------------------------------------

/// Response body for `GET /health`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HealthResponse {
    /// Overall service status: `"ok"` or `"degraded"`.
    pub status: String,
    /// Whether the DEK is currently loaded and ready.
    pub dek_ready: bool,
    /// Number of OpenAPI schemas currently cached.
    pub schemas_loaded: usize,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn encrypt_request_round_trip() {
        let req = EncryptRequest {
            payload: json!({"ssn": "123-45-6789", "name": "Alice"}),
        };
        let json = serde_json::to_string(&req).unwrap();
        let decoded: EncryptRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded.payload["ssn"], "123-45-6789");
    }

    #[test]
    fn error_response_new() {
        let e = ErrorResponse::new("bad_request", "missing schema header");
        assert_eq!(e.code, "bad_request");
        assert!(e.message.contains("missing schema header"));
    }

    #[test]
    fn health_response_serde() {
        let h = HealthResponse {
            status: "ok".into(),
            dek_ready: true,
            schemas_loaded: 3,
        };
        let json = serde_json::to_string(&h).unwrap();
        let decoded: HealthResponse = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded.schemas_loaded, 3);
    }
}
