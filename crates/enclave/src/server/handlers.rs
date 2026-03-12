//! Axum request handlers for all service endpoints.

use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use common::protocol::{
    DecryptRequest, DecryptResponse, EncryptRequest, EncryptResponse, ErrorResponse, HealthResponse,
};
use tracing::warn;

use super::state::AppState;
use crate::crypto::cipher::{decrypt_field, encrypt_field, CipherError, EncryptedField};
use crate::schema::PiiFieldPaths;

/// `POST /encrypt` — encrypt PII fields in the request payload.
///
/// The schema is identified by the value of the `X-Schema-Name` request header
/// (or the configured header name). PII fields are replaced with
/// `v1.<nonce>.<ciphertext>` strings.
pub async fn encrypt(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<EncryptRequest>,
) -> Response {
    use crate::telemetry::Metrics;
    let start = std::time::Instant::now();

    // Extract schema name from the configured header.
    let schema_name = match headers.get(state.schema_header_name.as_str()) {
        Some(v) => match v.to_str() {
            Ok(s) => s.to_owned(),
            Err(_) => {
                let err = ErrorResponse::new(
                    "bad_request",
                    format!(
                        "{} header contains non-ASCII characters",
                        state.schema_header_name
                    ),
                );
                let attrs = Metrics::error_attrs();
                state.metrics.encrypt_requests.add(1, &attrs);
                state
                    .metrics
                    .encrypt_latency_ms
                    .record(start.elapsed().as_secs_f64() * 1000.0, &attrs);
                return (StatusCode::BAD_REQUEST, Json(err)).into_response();
            }
        },
        None => {
            let err = ErrorResponse::new(
                "bad_request",
                format!("missing {} header", state.schema_header_name),
            );
            let attrs = Metrics::error_attrs();
            state.metrics.encrypt_requests.add(1, &attrs);
            state
                .metrics
                .encrypt_latency_ms
                .record(start.elapsed().as_secs_f64() * 1000.0, &attrs);
            return (StatusCode::BAD_REQUEST, Json(err)).into_response();
        }
    };

    // Resolve the schema from the cache.
    let cached = match state.schema_cache.get(&schema_name) {
        Ok(s) => s,
        Err(_) => {
            let err = ErrorResponse::new("bad_request", format!("unknown schema: {schema_name}"));
            let attrs = Metrics::error_attrs();
            state.metrics.encrypt_requests.add(1, &attrs);
            state
                .metrics
                .encrypt_latency_ms
                .record(start.elapsed().as_secs_f64() * 1000.0, &attrs);
            return (StatusCode::BAD_REQUEST, Json(err)).into_response();
        }
    };

    // Borrow the current DEK — 503 if not yet initialised.
    let dek = match state.dek_store.current().await {
        Ok(d) => d,
        Err(_) => {
            let err = ErrorResponse::new("service_unavailable", "DEK not yet initialised");
            let attrs = Metrics::error_attrs();
            state.metrics.encrypt_requests.add(1, &attrs);
            state
                .metrics
                .encrypt_latency_ms
                .record(start.elapsed().as_secs_f64() * 1000.0, &attrs);
            return (StatusCode::SERVICE_UNAVAILABLE, Json(err)).into_response();
        }
    };

    // Traverse and encrypt all PII fields in-place.
    let mut payload = req.payload;
    if let Err(e) = encrypt_pii_fields(&mut payload, &cached.pii_paths, &dek.0[..]) {
        warn!(error = %e, "encryption failed");
        let err = ErrorResponse::new("internal_error", "encryption failed");
        let attrs = Metrics::error_attrs();
        state.metrics.encrypt_requests.add(1, &attrs);
        state
            .metrics
            .encrypt_latency_ms
            .record(start.elapsed().as_secs_f64() * 1000.0, &attrs);
        return (StatusCode::INTERNAL_SERVER_ERROR, Json(err)).into_response();
    }

    let attrs = Metrics::success_attrs();
    state.metrics.encrypt_requests.add(1, &attrs);
    state
        .metrics
        .encrypt_latency_ms
        .record(start.elapsed().as_secs_f64() * 1000.0, &attrs);
    (StatusCode::OK, Json(EncryptResponse { payload })).into_response()
}

/// `GET /health` — liveness and readiness check.
///
/// Returns `200 OK` when the DEK is loaded and at least one schema is cached.
/// Returns `503 Service Unavailable` otherwise.
pub async fn health(State(state): State<AppState>) -> Response {
    let dek_ready = state.dek_store.is_ready().await;
    let schemas_loaded = state.schema_cache.len();

    let (status_code, status_str) = if dek_ready && schemas_loaded > 0 {
        (StatusCode::OK, "ok")
    } else {
        (StatusCode::SERVICE_UNAVAILABLE, "degraded")
    };

    let body = HealthResponse {
        status: status_str.into(),
        dek_ready,
        schemas_loaded,
    };
    (status_code, Json(body)).into_response()
}

/// `POST /decrypt` — decrypt PII fields in the request payload.
///
/// The schema is identified by the value of the `X-Schema-Name` request header
/// (or the configured header name). Fields at PII paths that carry an encrypted
/// `v1.<nonce>.<ciphertext>` value are decrypted back to plaintext. Fields that
/// are not in the `v1.` format are left unchanged.
pub async fn decrypt(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<DecryptRequest>,
) -> Response {
    use crate::telemetry::Metrics;
    let start = std::time::Instant::now();

    // Extract schema name from the configured header.
    let schema_name = match headers.get(state.schema_header_name.as_str()) {
        Some(v) => match v.to_str() {
            Ok(s) => s.to_owned(),
            Err(_) => {
                let err = ErrorResponse::new(
                    "bad_request",
                    format!(
                        "{} header contains non-ASCII characters",
                        state.schema_header_name
                    ),
                );
                let attrs = Metrics::error_attrs();
                state.metrics.decrypt_requests.add(1, &attrs);
                state
                    .metrics
                    .decrypt_latency_ms
                    .record(start.elapsed().as_secs_f64() * 1000.0, &attrs);
                return (StatusCode::BAD_REQUEST, Json(err)).into_response();
            }
        },
        None => {
            let err = ErrorResponse::new(
                "bad_request",
                format!("missing {} header", state.schema_header_name),
            );
            let attrs = Metrics::error_attrs();
            state.metrics.decrypt_requests.add(1, &attrs);
            state
                .metrics
                .decrypt_latency_ms
                .record(start.elapsed().as_secs_f64() * 1000.0, &attrs);
            return (StatusCode::BAD_REQUEST, Json(err)).into_response();
        }
    };

    // Resolve the schema from the cache.
    let cached = match state.schema_cache.get(&schema_name) {
        Ok(s) => s,
        Err(_) => {
            let err = ErrorResponse::new("bad_request", format!("unknown schema: {schema_name}"));
            let attrs = Metrics::error_attrs();
            state.metrics.decrypt_requests.add(1, &attrs);
            state
                .metrics
                .decrypt_latency_ms
                .record(start.elapsed().as_secs_f64() * 1000.0, &attrs);
            return (StatusCode::BAD_REQUEST, Json(err)).into_response();
        }
    };

    // Borrow the current DEK — 503 if not yet initialised.
    let dek = match state.dek_store.current().await {
        Ok(d) => d,
        Err(_) => {
            let err = ErrorResponse::new("service_unavailable", "DEK not yet initialised");
            let attrs = Metrics::error_attrs();
            state.metrics.decrypt_requests.add(1, &attrs);
            state
                .metrics
                .decrypt_latency_ms
                .record(start.elapsed().as_secs_f64() * 1000.0, &attrs);
            return (StatusCode::SERVICE_UNAVAILABLE, Json(err)).into_response();
        }
    };

    // Traverse and decrypt all PII fields in-place.
    let mut payload = req.payload;
    if let Err(e) = decrypt_pii_fields(&mut payload, &cached.pii_paths, &dek.0[..]) {
        warn!(error = %e, "decryption failed");
        let err = ErrorResponse::new("internal_error", "decryption failed");
        let attrs = Metrics::error_attrs();
        state.metrics.decrypt_requests.add(1, &attrs);
        state
            .metrics
            .decrypt_latency_ms
            .record(start.elapsed().as_secs_f64() * 1000.0, &attrs);
        return (StatusCode::INTERNAL_SERVER_ERROR, Json(err)).into_response();
    }

    let attrs = Metrics::success_attrs();
    state.metrics.decrypt_requests.add(1, &attrs);
    state
        .metrics
        .decrypt_latency_ms
        .record(start.elapsed().as_secs_f64() * 1000.0, &attrs);
    (StatusCode::OK, Json(DecryptResponse { payload })).into_response()
}

/// Catch-all 404 handler.
pub async fn not_found() -> impl IntoResponse {
    let err = ErrorResponse::new("not_found", "the requested resource does not exist");
    (StatusCode::NOT_FOUND, Json(err))
}

// ---------------------------------------------------------------------------
// PII field traversal helpers
// ---------------------------------------------------------------------------

/// Segments of a dot-notation PII field path.
enum PathSegment {
    /// Navigate into an object property by name.
    Key(String),
    /// Expand into every element of a JSON array.
    ArrayItem,
}

/// Parse a dot-notation PII path into a list of [`PathSegment`]s.
///
/// Array fields use the `[]` suffix before the dot separator, e.g.
/// `"orders[].card_number"` → `[Key("orders"), ArrayItem, Key("card_number")]`.
fn parse_path(path: &str) -> Vec<PathSegment> {
    let mut segments = Vec::new();
    for part in path.split('.') {
        if let Some(key) = part.strip_suffix("[]") {
            segments.push(PathSegment::Key(key.to_owned()));
            segments.push(PathSegment::ArrayItem);
        } else {
            segments.push(PathSegment::Key(part.to_owned()));
        }
    }
    segments
}

/// Recursively navigate `value` following `segments` and encrypt any string
/// leaf found at the end of the path.
fn encrypt_at_path(
    value: &mut serde_json::Value,
    segments: &[PathSegment],
    dek: &[u8],
) -> Result<(), CipherError> {
    if segments.is_empty() {
        if let serde_json::Value::String(s) = value {
            let encrypted = encrypt_field(s.as_bytes(), dek)?;
            *value = serde_json::Value::String(encrypted.to_string_repr());
        }
        return Ok(());
    }

    match &segments[0] {
        PathSegment::Key(key) => {
            if let serde_json::Value::Object(map) = value {
                if let Some(child) = map.get_mut(key) {
                    encrypt_at_path(child, &segments[1..], dek)?;
                }
            }
        }
        PathSegment::ArrayItem => {
            if let serde_json::Value::Array(arr) = value {
                for item in arr.iter_mut() {
                    encrypt_at_path(item, &segments[1..], dek)?;
                }
            }
        }
    }
    Ok(())
}

/// Encrypt all PII string fields in `payload` according to `pii_paths`.
fn encrypt_pii_fields(
    payload: &mut serde_json::Value,
    pii_paths: &PiiFieldPaths,
    dek: &[u8],
) -> Result<(), CipherError> {
    for path in pii_paths {
        let segments = parse_path(path);
        encrypt_at_path(payload, &segments, dek)?;
    }
    Ok(())
}

/// Recursively navigate `value` following `segments` and decrypt any string
/// leaf at the end of the path that carries the `v1.` ciphertext prefix.
/// Leaves that do not start with `v1.` are left unchanged.
fn decrypt_at_path(
    value: &mut serde_json::Value,
    segments: &[PathSegment],
    dek: &[u8],
) -> Result<(), CipherError> {
    if segments.is_empty() {
        if let serde_json::Value::String(s) = value {
            if s.starts_with("v1.") {
                let field = EncryptedField::from_str(s)?;
                let plaintext = decrypt_field(&field, dek)?;
                *value = serde_json::Value::String(
                    String::from_utf8(plaintext).map_err(|_| CipherError::AeadFailure)?,
                );
            }
            // Non-encrypted strings are left as-is (idempotent path traversal).
        }
        return Ok(());
    }

    match &segments[0] {
        PathSegment::Key(key) => {
            if let serde_json::Value::Object(map) = value {
                if let Some(child) = map.get_mut(key) {
                    decrypt_at_path(child, &segments[1..], dek)?;
                }
            }
        }
        PathSegment::ArrayItem => {
            if let serde_json::Value::Array(arr) = value {
                for item in arr.iter_mut() {
                    decrypt_at_path(item, &segments[1..], dek)?;
                }
            }
        }
    }
    Ok(())
}

/// Decrypt all PII string fields in `payload` according to `pii_paths`.
fn decrypt_pii_fields(
    payload: &mut serde_json::Value,
    pii_paths: &PiiFieldPaths,
    dek: &[u8],
) -> Result<(), CipherError> {
    for path in pii_paths {
        let segments = parse_path(path);
        decrypt_at_path(payload, &segments, dek)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::routing::get;
    use axum::{body::Body, http::Request, Router};
    use tower::ServiceExt;

    fn test_router() -> Router {
        Router::new()
            .route("/health", get(health))
            .with_state(AppState::default())
    }

    #[tokio::test]
    async fn health_returns_503_when_not_ready() {
        let app = test_router();
        let req = Request::builder()
            .uri("/health")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    #[test]
    fn parse_path_flat() {
        let segs = parse_path("ssn");
        assert!(matches!(segs[0], PathSegment::Key(ref k) if k == "ssn"));
        assert_eq!(segs.len(), 1);
    }

    #[test]
    fn parse_path_nested() {
        let segs = parse_path("user.address.zip");
        assert_eq!(segs.len(), 3);
    }

    #[test]
    fn parse_path_array() {
        let segs = parse_path("orders[].card_number");
        assert_eq!(segs.len(), 3);
        assert!(matches!(segs[1], PathSegment::ArrayItem));
    }

    #[test]
    fn encrypt_flat_field() {
        use crate::crypto::KEY_LEN;
        let dek = vec![0x42u8; KEY_LEN];
        let mut val = serde_json::json!({"ssn": "123-45-6789", "name": "Alice"});
        let mut paths = PiiFieldPaths::new();
        paths.insert("ssn".into());
        encrypt_pii_fields(&mut val, &paths, &dek).unwrap();
        let ssn = val["ssn"].as_str().unwrap();
        assert!(ssn.starts_with("v1."), "expected v1. prefix, got: {ssn}");
        assert_eq!(val["name"].as_str().unwrap(), "Alice");
    }

    #[test]
    fn encrypt_nested_field() {
        use crate::crypto::KEY_LEN;
        let dek = vec![0x42u8; KEY_LEN];
        let mut val = serde_json::json!({"user": {"address": {"zip": "90210"}}});
        let mut paths = PiiFieldPaths::new();
        paths.insert("user.address.zip".into());
        encrypt_pii_fields(&mut val, &paths, &dek).unwrap();
        let zip = val["user"]["address"]["zip"].as_str().unwrap();
        assert!(zip.starts_with("v1."));
    }

    #[test]
    fn encrypt_array_field() {
        use crate::crypto::KEY_LEN;
        let dek = vec![0x42u8; KEY_LEN];
        let mut val = serde_json::json!({
            "orders": [
                {"card_number": "4111111111111111"},
                {"card_number": "5500000000000004"}
            ]
        });
        let mut paths = PiiFieldPaths::new();
        paths.insert("orders[].card_number".into());
        encrypt_pii_fields(&mut val, &paths, &dek).unwrap();
        for order in val["orders"].as_array().unwrap() {
            let cn = order["card_number"].as_str().unwrap();
            assert!(cn.starts_with("v1."), "expected encrypted, got: {cn}");
        }
    }

    #[test]
    fn missing_field_is_noop() {
        use crate::crypto::KEY_LEN;
        let dek = vec![0x42u8; KEY_LEN];
        let mut val = serde_json::json!({"name": "Bob"});
        let mut paths = PiiFieldPaths::new();
        paths.insert("ssn".into());
        encrypt_pii_fields(&mut val, &paths, &dek).unwrap();
        // no panic, "name" untouched
        assert_eq!(val["name"].as_str().unwrap(), "Bob");
    }

    #[test]
    fn decrypt_flat_field_round_trip() {
        use crate::crypto::{cipher::encrypt_field, KEY_LEN};
        let dek = vec![0x42u8; KEY_LEN];
        let plaintext = "123-45-6789";
        let encrypted_field = encrypt_field(plaintext.as_bytes(), &dek).unwrap();
        let ciphertext_str = encrypted_field.to_string_repr();

        let mut val = serde_json::json!({"ssn": ciphertext_str, "name": "Alice"});
        let mut paths = PiiFieldPaths::new();
        paths.insert("ssn".into());
        decrypt_pii_fields(&mut val, &paths, &dek).unwrap();
        assert_eq!(val["ssn"].as_str().unwrap(), plaintext);
        assert_eq!(val["name"].as_str().unwrap(), "Alice");
    }

    #[test]
    fn decrypt_non_encrypted_field_is_noop() {
        use crate::crypto::KEY_LEN;
        let dek = vec![0x42u8; KEY_LEN];
        let mut val = serde_json::json!({"ssn": "plaintext-already", "name": "Bob"});
        let mut paths = PiiFieldPaths::new();
        paths.insert("ssn".into());
        // A non-v1. string at a PII path should be left unchanged.
        decrypt_pii_fields(&mut val, &paths, &dek).unwrap();
        assert_eq!(val["ssn"].as_str().unwrap(), "plaintext-already");
    }

    #[test]
    fn decrypt_nested_field_round_trip() {
        use crate::crypto::{cipher::encrypt_field, KEY_LEN};
        let dek = vec![0x42u8; KEY_LEN];
        let plaintext = "90210";
        let ciphertext_str = encrypt_field(plaintext.as_bytes(), &dek)
            .unwrap()
            .to_string_repr();

        let mut val = serde_json::json!({"user": {"address": {"zip": ciphertext_str}}});
        let mut paths = PiiFieldPaths::new();
        paths.insert("user.address.zip".into());
        decrypt_pii_fields(&mut val, &paths, &dek).unwrap();
        assert_eq!(val["user"]["address"]["zip"].as_str().unwrap(), plaintext);
    }

    #[test]
    fn decrypt_array_field_round_trip() {
        use crate::crypto::{cipher::encrypt_field, KEY_LEN};
        let dek = vec![0x42u8; KEY_LEN];
        let cards = ["4111111111111111", "5500000000000004"];
        let encrypted: Vec<String> = cards
            .iter()
            .map(|c| encrypt_field(c.as_bytes(), &dek).unwrap().to_string_repr())
            .collect();

        let mut val = serde_json::json!({
            "orders": [
                {"card_number": encrypted[0].clone()},
                {"card_number": encrypted[1].clone()}
            ]
        });
        let mut paths = PiiFieldPaths::new();
        paths.insert("orders[].card_number".into());
        decrypt_pii_fields(&mut val, &paths, &dek).unwrap();
        for (i, order) in val["orders"].as_array().unwrap().iter().enumerate() {
            assert_eq!(order["card_number"].as_str().unwrap(), cards[i]);
        }
    }

    #[test]
    fn encrypt_then_decrypt_idempotent() {
        use crate::crypto::KEY_LEN;
        let dek = vec![0x42u8; KEY_LEN];
        let original = serde_json::json!({"ssn": "123-45-6789", "name": "Alice"});
        let mut paths = PiiFieldPaths::new();
        paths.insert("ssn".into());

        let mut val = original.clone();
        encrypt_pii_fields(&mut val, &paths, &dek).unwrap();
        decrypt_pii_fields(&mut val, &paths, &dek).unwrap();
        assert_eq!(val, original);
    }
}
