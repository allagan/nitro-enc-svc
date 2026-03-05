# Encryption API Test Report — 2026-03-05

## Environment

| Item | Value |
|---|---|
| Enclave image | `runner:2e2ae31` |
| EKS cluster | `nitro-enc-svc-dev` (us-east-2) |
| NLB endpoint | `a91838215fffe476d835e33a202ccd7e-75f088c3cff040c2.elb.us-east-2.amazonaws.com:8443` |
| Schema under test | `payments-v1` (from S3) |
| Test method | `curl` via `kubectl exec` into test-client pod |

## PII Field Map — `payments-v1`

| Field path | `x-pii` | Expected |
|---|---|---|
| `card_number` | true | Encrypted |
| `card_holder_name` | true | Encrypted |
| `billing_address.street` | true | Encrypted |
| `billing_address.zip` | true | Encrypted |
| `billing_address.city` | false | Pass-through |
| `merchant_id` | false | Pass-through |
| `amount_cents` | false | Pass-through |

## Test Cases

### T-01 — Health Endpoint
**Request:** `GET /health`
**Response:** `200 {"status":"ok","dek_ready":true,"schemas_loaded":1}`
**Result: PASS** — DEK decrypted and schema cache populated at startup.

---

### T-02 — Full PII Payload
**Request:**
```json
{"payload":{"card_number":"4111111111111111","card_holder_name":"Jane Smith",
 "billing_address":{"street":"123 Main St","city":"Columbus","zip":"43215"},
 "merchant_id":"M001","amount_cents":9999}}
```
**Response:**
```json
{"payload":{
  "card_number":       "v1.AlNLxS87k1dKTj8s.n_lRwHN5NH0Vi6LYas7NUijhmc13X4Nwp-bGDL448HE",
  "card_holder_name":  "v1.Nl3LnvOyQljp-F9a.KYt8ZFZmr668phYcugvnVW4kC9f_ja3vfbc",
  "billing_address":{
    "street": "v1.bxVa7ohqxedNCLZ7.DR6_vi89tCVxTqE0rmVaTvNUemGdELCo3P3O",
    "city":   "Columbus",
    "zip":    "v1.uUyK58NAc7pG8mzi.N6kIC4OXkJBxhc3hvOloUORjLJMn"
  },
  "merchant_id":  "M001",
  "amount_cents": 9999
}}
```
**Result: PASS** — All 4 PII fields encrypted; 3 non-PII fields preserved; nested `city` (not `x-pii`) correctly excluded.

---

### T-03 — Encrypted Value Format
**Regex:** `^v1\.[A-Za-z0-9_-]+\.[A-Za-z0-9_-]+$`
**Sample value:** `v1.8JBj53FD_z51XNjT.88JI3r6W1v-QLdP5Fm3CUnsjmAUvgjyXGzjwVzIsZwY`
**Result: PASS** — All encrypted values conform to `v1.<base64url-nonce>.<base64url-ciphertext>` (URL-safe, no padding).

---

### T-04 — Determinism (Same Plaintext → Same Ciphertext?)
Two consecutive calls with identical input `{"payload":{"card_number":"4111111111111111",...}}`:

| Call | Encrypted `card_number` |
|---|---|
| 1 | `v1.AlNLxS87k1dKTj8s.n_lRwHN5NH0Vi6LYas7NUijhmc13X4Nwp-bGDL448HE` |
| 2 | `v1.eQnesVGScp7v1ZVe.jzIDd7abhkbZBeAra442_sOkwvyUZR6_w3giahZBp6A` |

The nonce component (second segment) differs between calls.

**Result: FAIL** — See Finding F-1.

---

### T-05 — Different Plaintexts → Different Ciphertexts
| Input | Encrypted `card_number` |
|---|---|
| `4111111111111111` | `v1.AlNLxS87k1dKTj8s.n_lRwH...` |
| `5500005555555559` | `v1.Tm1JDzJd_b8LshaY.wBZvoX...` |

**Result: PASS** — Different plaintexts produce structurally different ciphertexts.

---

### T-06 — Nested Object PII Fields
**Input:** `billing_address.street` = `"789 Elm Blvd"`, `.city` = `"Cincinnati"`, `.zip` = `"45201"`
**Output:** `street` and `zip` encrypted; `city` preserved as `"Cincinnati"`.
**Result: PASS** — Field-level x-pii annotation respected within nested objects.

---

### T-07 — Special Characters in PII Fields
**Input:** `card_holder_name` = `"O'Brien, Séan"` (apostrophe + Unicode)
**Output:** `"v1.2FQbtxBuuigiyncs.kMuWmHhRwlXbrGzyKnWyGhhce7QAqA3JXFAvhdtm"`
**Result: PASS** — Non-ASCII characters encrypted without errors.

---

### T-08 — Empty Payload
**Input:** `{"payload":{}}`
**Response:** `200 {"payload":{}}`
**Result: PASS** — No fields to encrypt; response mirrors input.

---

### T-09 — Missing `X-Schema-Name` Header
**Response:** `400 {"code":"bad_request","message":"missing X-Schema-Name header"}`
**Result: PASS** — Correct 400 with descriptive message.

---

### T-10 — Unknown Schema Name
**Header:** `X-Schema-Name: nonexistent-schema`
**Response:** `400 {"code":"bad_request","message":"unknown schema: nonexistent-schema"}`
**Result: PASS** — Schema name echoed in the error.

---

### T-11 — Invalid JSON Body
**Body:** `not-json`
**Response:** `400 Failed to parse the request body as JSON: expected ident at line 1 column 2`
**Result: PASS** — Parse error surfaced with HTTP 400.

---

### T-12 — Missing `payload` Wrapper
**Body:** `{"card_number":"4111111111111111"}` (no `payload` key)
**Response:** `422 Failed to deserialize the JSON body into the target type: missing field 'payload' at line 1 column 34`
**Result: PASS** — 422 with clear deserialization message.

---

### T-13 — Unknown Route
**Request:** `GET /nonexistent`
**Response:** `404 {"code":"not_found","message":"the requested resource does not exist"}`
**Result: PASS** — JSON 404 with correct error envelope.

---

### T-14 — Concurrent Load (10 parallel requests)
10 simultaneous `POST /encrypt` calls completed without errors or connection failures. Each response contained independently encrypted values with unique nonces.
**Result: PASS** — Multi-threaded tokio runtime handles concurrent requests correctly.

---

### T-15 — CloudWatch Log Delivery
Log group `/nitro-enc-svc/dev/enclave`, stream `enclave-logs`:
```
INFO  - nitro-enc-svc starting
INFO  - DEK fetched and stored successfully
INFO  - loaded schema from S3
INFO  - schema cache refreshed
INFO  - listening (TLS, vsock)
WARN  - TLS handshake failed  [repeated — NLB health probes, see F-3]
```
**Result: PASS** — Structured JSON logs flow from enclave → vsock bridge → OTEL Collector → CloudWatch.

---

## Results Summary

| Test | Result |
|---|---|
| T-01 Health endpoint | ✅ PASS |
| T-02 Full PII payload | ✅ PASS |
| T-03 Encrypted value format | ✅ PASS |
| T-04 Determinism | ❌ FAIL |
| T-05 Different plaintexts differ | ✅ PASS |
| T-06 Nested PII fields | ✅ PASS |
| T-07 Special characters | ✅ PASS |
| T-08 Empty payload | ✅ PASS |
| T-09 Missing schema header | ✅ PASS |
| T-10 Unknown schema name | ✅ PASS |
| T-11 Invalid JSON body | ✅ PASS |
| T-12 Missing payload wrapper | ✅ PASS |
| T-13 Unknown route | ✅ PASS |
| T-14 10 concurrent requests | ✅ PASS |
| T-15 CloudWatch log delivery | ✅ PASS |

**14 PASS / 1 FAIL**

---

## Findings

### F-1 — Non-Deterministic Encryption (design deviation) — CRITICAL

**Symptom:** Identical plaintext + DEK → different ciphertexts on every call.

**Root cause:** `crates/enclave/src/crypto/cipher.rs:encrypt_field()` generates a fresh random 12-byte nonce per call via `OsRng`:

```rust
// cipher.rs line ~100
let mut nonce_bytes = [0u8; NONCE_LEN];
OsRng.fill_bytes(&mut nonce_bytes);
```

The doc comment on the function even acknowledges this: _"the same plaintext + DEK will produce the same output only when the same nonce is reused — here each call generates a fresh nonce."_ This directly contradicts the CLAUDE.md design requirement:

> Same plaintext + same DEK → same ciphertext. Required for tokenization/lookup use cases.

**Security note:** The current approach is cryptographically secure — random nonces with AES-256-GCM-SIV are safe. The issue is a functional gap, not a security vulnerability. AES-GCM-SIV is specifically designed to be nonce-misuse resistant, so deriving the nonce deterministically (as required by the spec) is safe.

**Fix:** See action plan item A-1.

---

### F-2 — NLB Cross-Node Routing Causes Intermittent Timeouts — MEDIUM

**Symptom:** ~20–30% of curl requests from test pod timeout at 30s. Successful requests complete in <100ms.

**Root cause:** The `vsock-proxy-nlb` Service uses `externalTrafficPolicy: Cluster` (default), which registers all nodes as NLB targets via NodePort. When the NLB routes to the general-purpose node (10.0.53.90, which has no vsock-proxy pod), kube-proxy iptables rules forward the connection cross-node to the nitro node (10.0.52.83). This is the expected kube-proxy path but introduces intermittent connection failures in some network configurations with NLB health check timing.

**Fix:** See action plan item A-2.

---

### F-3 — NLB Health Check Noise in CloudWatch Logs — LOW

**Symptom:** `WARN TLS handshake failed` logs at ~6/minute from NLB health probes. These dominate the CloudWatch log stream and obscure meaningful application logs.

**Root cause:** The NLB performs TCP health checks (raw TCP connect, no TLS ClientHello) to port 8443. The vsock-proxy passes the raw bytes to the enclave over vsock. The enclave's TLS server expects a TLS ClientHello; when none arrives, it logs a warning and closes the connection. The NLB still marks the target healthy (TCP connect succeeded).

**Fix:** See action plan item A-3.
