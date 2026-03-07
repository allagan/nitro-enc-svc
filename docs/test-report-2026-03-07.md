# Encryption API Test Report — 2026-03-07

## Environment

| Item | Value |
|---|---|
| Enclave image | `runner:20288b0` |
| EKS cluster | `nitro-enc-svc-dev` (us-east-2) |
| NLB endpoint | `a91838215fffe476d835e33a202ccd7e-75f088c3cff040c2.elb.us-east-2.amazonaws.com:8443` |
| Schema under test | `payments-v1` (from S3) |
| Test method | `curl` via AWS SSM `send-command` to general node (i-0f59535d0c9dc6cfc) |
| Previous report | `docs/test-report-2026-03-05.md` (14 PASS / 1 FAIL) |

**Changes since last report:**
- A-1 fixed: deterministic AES-256-GCM-SIV nonce derivation via `HMAC-SHA256(DEK, plaintext)[0..12]`
- A-2 applied: `externalTrafficPolicy: Local` — NLB targets only the nitro node
- A-3 applied: OTEL Collector `filter/drop_nlb_noise` processor suppresses TLS handshake noise
- Test method corrected: NLB hairpinning prevented testing from the target node; all tests now run from the general node (10.0.53.90)

---

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

---

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
  "card_number":       "v1.LHs2Rlwz5dMSlPqu.knOjvIEhOYDnGBcVmmFfxdqMArorAjC4BFm6iAt8RiM",
  "card_holder_name":  "v1.Q0rDOIPYRlYNuWPS.KGUOQzVR_qYrwpVHbqBYAJYfbKBYoHV4Ig0",
  "billing_address":{
    "street": "v1.rif7baNM5Ii8fgVJ.pxbzIOvhXM1bKoQMFFPIZ_7t0kMkUnSGDynD",
    "city":   "Columbus",
    "zip":    "v1.F4emuBlpxzCSC7y4.ZWSeY93x2vSpFqWQmNLMOkew6Hc5"
  },
  "merchant_id":  "M001",
  "amount_cents": 9999
}}
```
**Result: PASS** — All 4 PII fields encrypted; 3 non-PII fields preserved; nested `city` correctly excluded.

---

### T-03 — Encrypted Value Format
**Regex:** `^v1\.[A-Za-z0-9_-]+\.[A-Za-z0-9_-]+$`
**Sample value:** `v1.LHs2Rlwz5dMSlPqu.knOjvIEhOYDnGBcVmmFfxdqMArorAjC4BFm6iAt8RiM`
**Result: PASS** — All encrypted values conform to `v1.<base64url-nonce>.<base64url-ciphertext>` (URL-safe, no padding).

---

### T-04 — Determinism (Same Plaintext → Same Ciphertext)
Three consecutive calls with identical input:

| Call | Encrypted `card_number` |
|---|---|
| 1 | `v1.LHs2Rlwz5dMSlPqu.knOjvIEhOYDnGBcVmmFfxdqMArorAjC4BFm6iAt8RiM` |
| 2 | `v1.LHs2Rlwz5dMSlPqu.knOjvIEhOYDnGBcVmmFfxdqMArorAjC4BFm6iAt8RiM` |
| 3 | `v1.LHs2Rlwz5dMSlPqu.knOjvIEhOYDnGBcVmmFfxdqMArorAjC4BFm6iAt8RiM` |

All four PII fields (`card_number`, `card_holder_name`, `street`, `zip`) are byte-identical across all three responses.

**Result: PASS** ✅ — Fixed from FAIL in previous report. `HMAC-SHA256(DEK, plaintext)[0..12]` nonce derivation produces the same nonce for the same input, satisfying the tokenisation guarantee.

---

### T-05 — Different Plaintexts → Different Ciphertexts
| Input | Encrypted `card_number` |
|---|---|
| `4111111111111111` | `v1.LHs2Rlwz5dMSlPqu.knOjvIEhOYDnGBcVmmFfx...` |
| `5500005555555559` | `v1.4Q-sjamE2yPdB3bE.C8h17S2-B0ZG1eK5ybmQ0m...` |

**Result: PASS** — Different plaintexts produce distinct nonces and distinct ciphertexts.

---

### T-06 — Nested Object PII Fields
**Input:** `billing_address.street` = `"789 Elm Blvd"`, `.city` = `"Cincinnati"`, `.zip` = `"45201"`
**Output:** `street` and `zip` encrypted; `city` preserved as `"Cincinnati"`.
**Result: PASS** — Field-level `x-pii` annotation respected within nested objects.

---

### T-07 — Special Characters in PII Fields
**Input:** `card_holder_name` = `"O'Brien, Séan"` (apostrophe + Unicode)
**Output:** `"v1.XoK5nEgvfoQfF65B.2lINCGDo9TR8Jrud80JERcXseJtdgEelvn10N6-EgAs"`
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
**Header:** `X-Schema-Name: no-such-schema`
**Response:** `400 {"code":"bad_request","message":"unknown schema: no-such-schema"}`
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
10 simultaneous `POST /encrypt` calls fired in parallel. All 10 completed successfully with valid encrypted responses — 0 errors.

**Result: PASS** — Multi-threaded tokio runtime handles concurrent requests correctly.

---

### T-14b — Concurrent Determinism
All 10 concurrent responses for the same input payload are byte-identical — 0 mismatches.

**Result: PASS** — Determinism holds under concurrent load (HMAC-SHA256 nonce is stateless, no lock contention on the encryption path).

---

### T-15 — CloudWatch Log Delivery
Log group `/nitro-enc-svc/dev/enclave`, stream `enclave-logs`: confirmed active and receiving events from the enclave.

**Result: PASS** — Structured JSON logs flow from enclave → vsock bridge → OTEL Collector → CloudWatch. NLB health check noise (`TLS handshake failed`) suppressed by `filter/drop_nlb_noise` processor (A-3).

---

## Results Summary

| Test | Description | Result | vs. 2026-03-05 |
|---|---|---|---|
| T-01 | Health endpoint | ✅ PASS | — |
| T-02 | Full PII payload | ✅ PASS | — |
| T-03 | Encrypted value format | ✅ PASS | — |
| T-04 | Determinism | ✅ PASS | **FIXED** (was FAIL) |
| T-05 | Different plaintexts differ | ✅ PASS | — |
| T-06 | Nested PII fields | ✅ PASS | — |
| T-07 | Special characters | ✅ PASS | — |
| T-08 | Empty payload | ✅ PASS | — |
| T-09 | Missing schema header | ✅ PASS | — |
| T-10 | Unknown schema name | ✅ PASS | — |
| T-11 | Invalid JSON body | ✅ PASS | — |
| T-12 | Missing payload wrapper | ✅ PASS | — |
| T-13 | Unknown route | ✅ PASS | — |
| T-14 | 10 concurrent requests | ✅ PASS | — |
| T-14b | Concurrent determinism | ✅ PASS | **NEW** |
| T-15 | CloudWatch log delivery | ✅ PASS | — |

**16 PASS / 0 FAIL**

---

## Resolved Findings

All three findings from the 2026-03-05 report have been resolved:

| Finding | Severity | Resolution | Commit |
|---|---|---|---|
| F-1: Non-deterministic encryption | CRITICAL | HMAC-SHA256 nonce derivation in `cipher.rs` | `f6b81c8` |
| F-2: NLB cross-node routing timeouts | MEDIUM | `externalTrafficPolicy: Local` in Service spec | `e636981` |
| F-3: NLB health check log noise | LOW | `filter/drop_nlb_noise` in otel-collector.yaml | `e636981` |

**Note on F-2:** The original "timeouts" investigation also identified an AWS NLB hairpinning limitation: EC2 instances registered as NLB targets cannot connect back through that NLB. Tests must be run from a non-target node (general node 10.0.53.90) or an external client.
