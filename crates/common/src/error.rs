//! Common error types shared across crates.

use thiserror::Error;

/// Top-level service error type.
///
/// Variants map to HTTP status codes returned to callers:
/// - [`ServiceError::BadRequest`] → 400
/// - [`ServiceError::EncryptionFailure`] → 500
/// - [`ServiceError::Unavailable`] → 503
#[derive(Debug, Error)]
pub enum ServiceError {
    /// The request was malformed — missing header, unknown schema, or invalid JSON.
    #[error("bad request: {0}")]
    BadRequest(String),

    /// Encryption or decryption failed due to a crypto-layer error.
    #[error("encryption failure: {0}")]
    EncryptionFailure(String),

    /// A required resource (DEK, schema) is not yet initialised or is temporarily unavailable.
    #[error("service unavailable: {0}")]
    Unavailable(String),

    /// An unexpected internal error occurred.
    #[error("internal error: {0}")]
    Internal(String),
}

impl ServiceError {
    /// Returns the HTTP status code that should be sent for this error.
    pub fn http_status(&self) -> u16 {
        match self {
            ServiceError::BadRequest(_) => 400,
            ServiceError::EncryptionFailure(_) => 500,
            ServiceError::Unavailable(_) => 503,
            ServiceError::Internal(_) => 500,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn http_status_codes() {
        assert_eq!(ServiceError::BadRequest("x".into()).http_status(), 400);
        assert_eq!(
            ServiceError::EncryptionFailure("x".into()).http_status(),
            500
        );
        assert_eq!(ServiceError::Unavailable("x".into()).http_status(), 503);
        assert_eq!(ServiceError::Internal("x".into()).http_status(), 500);
    }

    #[test]
    fn display_includes_message() {
        let e = ServiceError::BadRequest("missing schema header".into());
        assert!(e.to_string().contains("missing schema header"));
    }
}
