# nitro-enc-svc — Project Guide for Claude Code

## Overview

`nitro-enc-svc` is a high-throughput, low-latency PII field encryption service running inside
AWS Nitro Enclaves on EKS nodes. It receives HTTPS requests (TLS terminating inside the enclave),
parses JSON payloads against OpenAPI specs to identify PII fields, encrypts those fields using a
cached Data Encryption Key (DEK), and returns the transformed payload.

Target performance: **1000s TPS**, **single-digit millisecond latency**.

---

## Architecture

```
Client ──HTTPS──► NLB ──► [EKS Pod: vsock-proxy sidecar]
                                      │  HTTPS over vsock
                                      ▼
                             [Nitro Enclave: nitro-enc-svc]
                             • TLS terminates here (ACM for Nitro)
                             • Selects OpenAPI spec from X-Schema-Name header
                             • Identifies and encrypts PII fields (AES-256-GCM-SIV)
                             • Returns encrypted JSON payload
                                      │  vsock
                             [EKS Pod: vsock-proxy sidecar]
                                      │  HTTP (pod-local)
                                      ▼
                             [EKS Pod: Main App Container]
                                      │  calls downstream
                                      ▼
                             [Downstream Services]
                                      │  response path reverses back to NLB ──► Client

Enclave ──vsock──► [Parent EC2: aws-vsock-proxy] ──► AWS KMS
                                                 ──► AWS Secrets Manager
                                                 ──► AWS S3 (OpenAPI schemas)
                                                 ──► ACM for Nitro Enclaves

Enclave ──vsock──► [Parent EC2: OTEL Collector] ──► Observability backend
```

### Deployment topology

- **One Nitro Enclave per EKS node** (DaemonSet pattern).
- `vsock-proxy` runs as a **sidecar container** in every EKS Pod that needs encryption.
- Vsock communication is always node-local (Pod → same-node enclave).
- The `aws-vsock-proxy` and OTEL Collector run on the **parent EC2 instance** (not inside the enclave).

---

## Repository Structure

```
nitro-enc-svc/
├── CLAUDE.md
├── Cargo.toml                  # Workspace root
├── crates/
│   ├── enclave/                # Service running inside the Nitro Enclave
│   │   ├── Cargo.toml
│   │   └── src/
│   │       ├── main.rs
│   │       ├── config.rs       # Configuration loading and validation
│   │       ├── server/         # Axum HTTPS server, routing, middleware
│   │       ├── crypto/         # AES-256-GCM-SIV DEK-based field encryption
│   │       ├── dek/            # DEK fetch, decrypt, cache, background rotation
│   │       ├── schema/         # OpenAPI spec loading from S3, PII field resolution
│   │       ├── aws/            # AWS SDK clients (KMS, Secrets Manager, S3) via vsock
│   │       └── telemetry/      # OTEL setup: metrics, traces, structured logs via vsock
│   ├── vsock-proxy/            # Sidecar: TCP ↔ vsock tunnel for EKS pods
│   │   ├── Cargo.toml
│   │   └── src/
│   │       ├── main.rs
│   │       ├── config.rs
│   │       ├── proxy.rs        # Bidirectional TCP ↔ vsock forwarding
│   │       └── telemetry/
│   └── common/                 # Shared types, protocol definitions, errors
│       ├── Cargo.toml
│       └── src/
│           ├── lib.rs
│           ├── protocol.rs     # Request/response types shared across crates
│           └── error.rs        # Common error types
├── schemas/                    # Local OpenAPI spec files for dev/test only
├── config/                     # Environment-specific config files
├── deploy/
│   ├── daemonset-enclave.yaml  # Nitro Enclave DaemonSet (per node)
│   └── pod-example.yaml        # Example Pod spec with vsock-proxy sidecar
└── tests/                      # Integration tests (run against local mock enclave)
```

---

## Key Design Decisions

### 1. Deterministic Authenticated Encryption — AES-256-GCM-SIV

- Algorithm: **AES-256-GCM-SIV** (RFC 8452), via the `aes-gcm-siv` RustCrypto crate.
- Same plaintext + same DEK → same ciphertext. Required for tokenization/lookup use cases.
- **Do NOT use plain AES-256-GCM with a fixed or derived nonce.** GCM nonce reuse is
  catastrophic (breaks both confidentiality and authentication). AES-GCM-SIV is specifically
  designed for deterministic use and is nonce-misuse resistant.
- Encrypted field format (base64url, no padding):
  ```
  v1.<base64url(nonce)>.<base64url(ciphertext+tag)>
  ```
  The `v1` prefix enables future algorithm/key-version migration.

### 2. DEK Lifecycle

- At enclave startup: fetch envelope-encrypted DEK from **AWS Secrets Manager** → decrypt via
  **AWS KMS** (KMS key policy enforces Nitro attestation — PCR values must match).
- Decrypted DEK lives **only in enclave memory**, never written to disk.
- A **background Tokio task** periodically re-fetches and rotates the cached DEK.
  Rotation interval is configurable (`DEK_ROTATION_INTERVAL_SECS`, default: 3600).
- Encryption requests always read the current cached DEK via an `Arc<RwLock<Dek>>`.

### 3. OpenAPI Schema-Driven PII Field Selection

- Multiple OpenAPI spec files stored in **S3** (`S3_BUCKET` / `S3_PREFIX`).
- Incoming request carries a schema identifier in an HTTP header (default: `X-Schema-Name`,
  configurable via `SCHEMA_HEADER_NAME`).
- Schemas are loaded at startup and cached. A background task refreshes them periodically
  (`SCHEMA_REFRESH_INTERVAL_SECS`, default: 300).
- PII fields are identified via an OpenAPI extension: `x-pii: true` on schema properties.
- Field paths support nested objects and arrays (e.g., `user.address.ssn`, `orders[].card_number`).

### 4. TLS — ACM for Nitro Enclaves

- TLS terminates **inside the enclave** using a certificate managed by
  **AWS Certificate Manager (ACM) for Nitro Enclaves**.
- The ACM for Nitro Enclaves integration runs on the parent EC2 instance and delivers the
  private key+cert to the enclave over vsock, bound to the enclave's attestation document.
- TLS library: **rustls** (no OpenSSL dependency for enclave builds).

### 5. AWS API Access from Inside the Enclave

- The enclave has no direct network access. All AWS API calls (KMS, Secrets Manager, S3) go
  through the **aws-vsock-proxy** running on the parent EC2 instance.
- Configure the AWS SDK within the enclave to use vsock endpoints pointing to the proxy.

### 6. Observability — OTEL via Vsock

- OTEL SDK inside the enclave exports via **OTLP/gRPC** through vsock to the parent EC2.
- Parent EC2 runs an **OTEL Collector** that receives telemetry and exports to the backend.
- Structured logging: `tracing` crate with `tracing-opentelemetry` for trace correlation.
- **Metrics**: request count, latency histograms (p50/p95/p99), DEK age, schema cache hits/misses.
- **Traces**: per-request spans covering schema resolution, field traversal, encryption, response.
- **Logs**: structured JSON, log level configurable, sensitive data never logged.

### 7. Vsock-Proxy Sidecar

- Listens on a configurable TCP port for incoming HTTPS connections from the NLB.
- Establishes a vsock connection to the enclave (CID and port configurable).
- Performs bidirectional byte-stream forwarding (TCP ↔ vsock). TLS bytes are forwarded
  opaquely — TLS terminates in the enclave, not in the sidecar.
- After the enclave returns the encrypted payload, the sidecar forwards the HTTP response
  to the main app container (pod-local HTTP).
- The main app's response (from downstream) is relayed back through the sidecar to the NLB.

---

## Technology Stack

| Concern | Crate(s) |
|---|---|
| Async runtime | `tokio` (multi-thread scheduler) |
| HTTP server (enclave) | `axum`, `hyper` |
| TLS | `rustls`, `tokio-rustls` |
| Vsock (async) | `tokio-vsock` |
| Encryption | `aes-gcm-siv` (RustCrypto) |
| JSON | `serde`, `serde_json` |
| OpenAPI parsing | `openapiv3` |
| AWS SDK | `aws-sdk-kms`, `aws-sdk-secretsmanager`, `aws-sdk-s3`, `aws-config` |
| OTEL | `opentelemetry`, `opentelemetry-otlp`, `opentelemetry-sdk` |
| Tracing/logging | `tracing`, `tracing-subscriber`, `tracing-opentelemetry` |
| Configuration | `config` + `serde` |
| Error handling | `thiserror` (library errors), `anyhow` (application/bin errors) |
| Testing | `tokio::test`, `mockall`, `axum-test` |

---

## Configuration Reference

All configuration is loaded from environment variables (with optional config file overlay).

### Enclave (`crates/enclave`)

| Variable | Default | Description |
|---|---|---|
| `SECRET_ARN` | required | Secrets Manager ARN of the envelope-encrypted DEK |
| `KMS_KEY_ID` | required | KMS key ID used to decrypt the DEK |
| `S3_BUCKET` | required | S3 bucket containing OpenAPI spec files |
| `S3_PREFIX` | `schemas/` | S3 key prefix for OpenAPI spec files |
| `SCHEMA_HEADER_NAME` | `X-Schema-Name` | HTTP header used for schema selection |
| `DEK_ROTATION_INTERVAL_SECS` | `3600` | How often to refresh the cached DEK |
| `SCHEMA_REFRESH_INTERVAL_SECS` | `300` | How often to refresh cached OpenAPI schemas |
| `VSOCK_PROXY_CID` | required | Vsock CID of the parent EC2 aws-vsock-proxy |
| `VSOCK_PROXY_PORT` | `8000` | Vsock port of the aws-vsock-proxy |
| `TLS_PORT` | `443` | Port the enclave HTTPS server listens on |
| `OTEL_EXPORTER_OTLP_ENDPOINT` | required | OTLP endpoint (vsock address to OTEL collector) |
| `LOG_LEVEL` | `info` | Tracing log level |

### Vsock-Proxy (`crates/vsock-proxy`)

| Variable | Default | Description |
|---|---|---|
| `LISTEN_PORT` | `8443` | TCP port to accept incoming HTTPS connections |
| `ENCLAVE_CID` | required | Vsock CID of the Nitro Enclave on this node |
| `ENCLAVE_PORT` | `443` | Vsock port the enclave TLS server listens on |
| `MAIN_APP_ADDR` | required | Address of the main app container (pod-local HTTP) |
| `LOG_LEVEL` | `info` | Tracing log level |

---

## Code Conventions

### General

- All `pub` items must have doc comments (`///`).
- Use `thiserror` for all error types in library code; never use `unwrap()` or `expect()` in
  non-test code (use `?` propagation or explicit error handling).
- Never log, trace, or include plaintext PII or key material in any output.
- Prefer `Arc<T>` for shared state; avoid `Mutex` on the hot path — use `RwLock` where
  reads dominate, or lock-free structures where appropriate.
- Configuration must be validated at startup; fail fast with a clear error rather than
  discovering misconfiguration at runtime.

### Performance

- Avoid allocations on the hot encryption path. Pre-allocate buffers where possible.
- Schema lookups and DEK reads must not block the Tokio executor; use `RwLock` async variants.
- Target p99 latency < 5ms for the encryption path (excluding network).

### Testing

- Unit tests live in the same file as the code under test (`#[cfg(test)]` module).
- Integration tests live in `tests/` and run against a mock enclave or local server.
- Every public function must have at least one unit test.
- Use `mockall` for mocking AWS SDK traits in unit tests.
- Fuzz test the OpenAPI field traversal logic and the encryption/decryption round-trip.

### Modules

- One concept per module. Keep modules small and focused.
- `crypto/` must have no AWS or HTTP dependencies.
- `dek/` orchestrates AWS + crypto; `crypto/` provides the primitives.
- `schema/` must have no crypto or AWS KMS dependencies.

---

## Development Commands

```bash
# Build all crates
cargo build --workspace

# Run all tests
cargo test --workspace

# Run clippy (must pass with zero warnings)
cargo clippy --workspace --all-targets -- -D warnings

# Check formatting
cargo fmt --check

# Apply formatting
cargo fmt

# Build enclave binary (release, for EIF packaging)
cargo build --release -p enclave

# Build vsock-proxy binary
cargo build --release -p vsock-proxy

# Run enclave locally (without vsock, for unit testing server logic)
cargo run -p enclave -- --local-mode

# Generate OpenAPI docs
cargo doc --workspace --no-deps --open
```

---

## Encryption API

### POST /encrypt

Encrypts PII fields in the JSON body according to the OpenAPI spec identified by the
`X-Schema-Name` header (or the configured header name).

**Request headers:**
```
Content-Type: application/json
X-Schema-Name: payments-v1
```

**Request body:** Any JSON object matching the schema.

**Response:** Same JSON structure with PII fields replaced by encrypted values:
```json
{
  "name": "John Smith",
  "ssn": "v1.<nonce>.<ciphertext>",
  "card_number": "v1.<nonce>.<ciphertext>"
}
```

**Error responses:**
- `400` — missing or unknown schema name, JSON parse failure
- `500` — encryption failure, DEK unavailable
- `503` — schema not yet loaded, DEK not yet initialized

---

## Future Work (Out of Scope for v1)

- `POST /decrypt` endpoint for field-level decryption
- Multi-key support (key versioning in the `v1.<...>` prefix)
- Asymmetric encryption (RSA/EC) for specific field types
- gRPC transport option alongside REST
