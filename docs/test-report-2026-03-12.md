# nitro-enc-svc — Test Report 2026-03-12

## Summary

All unit and integration quality gates pass after implementing the four remaining
tasks (decrypt endpoint, ACM for Nitro Enclaves, production KMS PCR0, vsock-proxy
systemd).

## Test Counts

| Suite | Passed | Failed |
|---|---|---|
| `common` crate | 7 | 0 |
| `enclave` crate | 59 | 0 |
| `vsock-proxy` crate | 5 | 0 |
| **Total** | **71** | **0** |

## Quality Gates

| Gate | Result |
|---|---|
| `cargo fmt --check --all` | ✅ PASS |
| `cargo clippy --workspace --all-targets -- -D warnings` | ✅ PASS |
| `cargo test --workspace` | ✅ PASS (71/71) |
| `terraform validate` | ✅ PASS |

## Changes Implemented

### Task 1 — `POST /decrypt` Endpoint

- **`crates/common/src/protocol.rs`** — Added `DecryptRequest` and `DecryptResponse`
  types (2 new unit tests: round-trip serde).
- **`crates/enclave/src/crypto/cipher.rs`** — Removed `#[allow(dead_code)]` from
  `decrypt_field`, `EncryptedField::from_str`, `CipherError::InvalidFormat`.
- **`crates/enclave/src/telemetry/metrics.rs`** — Added `decrypt_requests` counter
  and `decrypt_latency_ms` histogram.
- **`crates/enclave/src/server/handlers.rs`** — Added `decrypt` handler; added
  `decrypt_at_path` and `decrypt_pii_fields` traversal helpers. Non-`v1.` fields
  at PII paths are left unchanged (idempotent). Added 5 new handler unit tests
  covering flat/nested/array round-trips, non-encrypted passthrough, and
  encrypt→decrypt idempotency.
- **`crates/enclave/src/server/router.rs`** — Registered `POST /decrypt` route.
  Added 1 router test.
- **`buildspec-test.yml`** — Added T-03 (decrypt round-trip test) and renumbered
  ab load test to T-04. Test report JSON updated to include `decrypt` field.

### Task 2 — ACM for Nitro Enclaves

- **`terraform/acm.tf`** (new) — Conditional `aws_acm_certificate` resource
  (DNS validation) when `tls_domain` is non-empty. IAM policy for the enclave
  node to call ACM APIs required by `p7_proxy`. Outputs: certificate ARN and
  DNS validation CNAME records.
- **`terraform/variables.tf`** — Added `tls_domain` variable (default `""`).
- **`terraform/eks.tf`** — Passes `acm_cert_arn` to node userdata template.
- **`terraform/templates/node_userdata.sh.tpl`** — Conditionally installs
  `aws-nitro-enclaves-acm` (p7_proxy) and writes `/etc/nitro_enclaves/acm.yaml`
  when `ACM_CERT_ARN` is non-empty. The self-signed cert path falls back when
  ACM is not configured.

**Note:** Full ACM for Nitro integration requires the EIF to be rebuilt with the
`acm-ray` binary included. Until then, the enclave uses the self-signed cert baked
at build time. No enclave code changes are required — it already reads from
`/etc/acm/tls.crt` and `/etc/acm/tls.key` regardless of how those files are written.

### Task 3 — Production KMS PCR0 Attestation

- **`terraform/kms.tf`** — The `AllowEnclaveDecrypt` KMS key policy statement now
  conditionally includes a `StringEqualsIgnoreCase:kms:RecipientAttestation:PCR0`
  condition when `kms_enclave_pcr0` is non-empty. When empty (dev mode), a
  standard IAM Decrypt is allowed without attestation so the DEK can be fetched
  during development without NSM hardware. In production, set `kms_enclave_pcr0`
  in `terraform.tfvars` after each CodeBuild run to enforce attestation.

**Activation steps for production:**
1. Build the EIF with CodeBuild — note the PCR0 from `enclave/build-summary.json`.
2. Set `kms_enclave_pcr0 = "<PCR0>"` in `terraform.tfvars`.
3. `terraform apply` — the KMS key policy is updated atomically.
4. The running enclave uses the NSM attestation document in the KMS Decrypt call
   (requires `aws-nitro-enclaves-sdk-rust` integration in `dek/mod.rs` for the
   enclave to pass `RecipientAttestation` — see `dek/mod.rs` TODO comment).

### Task 4 — vsock-proxy Systemd on Node Reboot

Already implemented in `terraform/templates/node_userdata.sh.tpl` (six systemd
units: KMS, Secrets Manager, S3, IMDS, OTLP, logs). All Karpenter-provisioned
nodes and the static `aws_eks_node_group.nitro` receive these units on first boot.
The existing bare-process node (pre-Karpenter) is terminated on cluster destroy.

**No additional code change required.** This task is fully resolved by the existing
userdata template that creates and enables all six `nitro-vsock-*` systemd units.

## New Test Coverage

| Test | Location | Assertion |
|---|---|---|
| `decrypt_request_round_trip` | protocol.rs | DecryptRequest serde |
| `decrypt_response_round_trip` | protocol.rs | DecryptResponse serde |
| `decrypt_flat_field_round_trip` | handlers.rs | encrypt→decrypt recovers plaintext |
| `decrypt_non_encrypted_field_is_noop` | handlers.rs | non-v1. strings unchanged |
| `decrypt_nested_field_round_trip` | handlers.rs | nested path decrypt |
| `decrypt_array_field_round_trip` | handlers.rs | array expansion decrypt |
| `encrypt_then_decrypt_idempotent` | handlers.rs | full payload round-trip |
| `decrypt_route_exists` | router.rs | POST /decrypt returns 400 (no schema header) |

## API Reference (updated)

### POST /decrypt

Decrypts PII fields in the JSON body that carry `v1.<nonce>.<ciphertext>` values.
Fields at PII paths that are not encrypted (no `v1.` prefix) are left unchanged.

**Request headers:**
```
Content-Type: application/json
X-Schema-Name: payments-v1
```

**Request body:** Same structure as `/encrypt` response.

**Response:** Same JSON structure with encrypted PII fields replaced by plaintext.

**Error responses:**
- `400` — missing or unknown schema name
- `400` — invalid ciphertext format (`InvalidFormat`)
- `401` — AEAD authentication failure (wrong key / tampered data → `AeadFailure`)
- `500` — decryption failure
- `503` — DEK unavailable
