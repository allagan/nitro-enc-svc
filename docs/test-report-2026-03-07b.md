# Encryption API Test Report — 2026-03-07 (OTEL Metrics + Benchmark Rollout)

## Environment

| Item | Value |
|---|---|
| Enclave image | `runner:1910780` |
| EKS cluster | `nitro-enc-svc-dev` (us-east-2) |
| NLB endpoint | `a91838215fffe476d835e33a202ccd7e-75f088c3cff040c2.elb.us-east-2.amazonaws.com:8443` |
| Schema under test | `payments-v1` (from S3) |
| Test method | `curl` + `ab` via AWS SSM `send-command` to general node (i-0f59535d0c9dc6cfc) |
| Previous report | `docs/test-report-2026-03-07.md` (16/16 PASS on image 20288b0) |

**Changes since last report (ecb180f → 1910780):**
- OTEL metrics pipeline added: `SdkMeterProvider` with OTLP/gRPC exporter (15 s export interval)
- New `telemetry/metrics.rs`: `encrypt_requests` counter, `encrypt_latency_ms` histogram, `dek_rotations` counter
- `/encrypt` handler instrumented — every exit path (success + all error paths) records counter + latency
- `rotation_task` increments `dek_rotations` on each successful background rotation
- OTEL Collector updated: `awsemf` exporter → CloudWatch namespace `NitroEncSvc/Dev`
- CloudWatch dashboard `NitroEncSvc-Dev` created with 7 widgets (fixed: OTelLib dimension, logGroupNames, Average/Maximum latency stats)
- **HTTP/1.1 keep-alive fix**: Added `alpn_protocols = ["http/1.1", "h2"]` to rustls ServerConfig
- **CompressionLayer removed**: was forcing `Transfer-Encoding: chunked` (no `Content-Length`), breaking keep-alive and load testers

**Unit test suite:** 63/63 PASS (`cargo test --workspace`)

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
**Response:**
```json
{"status":"ok","dek_ready":true,"schemas_loaded":1}
```
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
  "amount_cents": 9999,
  "billing_address": {
    "city":   "Columbus",
    "street": "v1.rif7baNM5Ii8fgVJ.pxbzIOvhXM1bKoQMFFPIZ_7t0kMkUnSGDynD",
    "zip":    "v1.F4emuBlpxzCSC7y4.ZWSeY93x2vSpFqWQmNLMOkew6Hc5"
  },
  "card_holder_name": "v1.Q0rDOIPYRlYNuWPS.KGUOQzVR_qYrwpVHbqBYAJYfbKBYoHV4Ig0",
  "card_number":      "v1.LHs2Rlwz5dMSlPqu.knOjvIEhOYDnGBcVmmFfxdqMArorAjC4BFm6iAt8RiM",
  "merchant_id":      "M001"
}}
```
**Result: PASS** — 4 PII fields encrypted; `city`, `merchant_id`, `amount_cents` preserved; nested non-PII `city` correctly excluded.

---

### T-03 — Missing Schema Header → 400
**Request:** `POST /encrypt` — no `X-Schema-Name` header
**Response:**
```json
{"code":"bad_request","message":"missing X-Schema-Name header"}
```
**Result: PASS** — Correct 400 with descriptive message. `error` counter incremented.

---

### T-04 — Unknown Schema → 400
**Request:** `POST /encrypt` with `X-Schema-Name: nonexistent`
**Response:**
```json
{"code":"bad_request","message":"unknown schema: nonexistent"}
```
**Result: PASS** — Correct 400. `error` counter incremented.

---

### T-05 — Determinism (Same Plaintext → Same Ciphertext)
Three consecutive calls with `card_number: 4111111111111111`:

| Call | Encrypted `card_number` |
|---|---|
| 1 | `v1.LHs2Rlwz5dMSlPqu.knOjvIEhOYDnGBcVmmFfxdqMArorAjC4BFm6iAt8RiM` |
| 2 | `v1.LHs2Rlwz5dMSlPqu.knOjvIEhOYDnGBcVmmFfxdqMArorAjC4BFm6iAt8RiM` |
| 3 | `v1.LHs2Rlwz5dMSlPqu.knOjvIEhOYDnGBcVmmFfxdqMArorAjC4BFm6iAt8RiM` |

**Result: PASS** — All three calls produce identical ciphertext. AES-256-GCM-SIV determinism confirmed.

---

### T-06 — Encrypted Value Format
**Regex:** `^v1\.[A-Za-z0-9_-]+\.[A-Za-z0-9_-]+$`
**Sample:** `v1.LHs2Rlwz5dMSlPqu.knOjvIEhOYDnGBcVmmFfxdqMArorAjC4BFm6iAt8RiM`
**Result: PASS** — URL-safe base64url, no padding, `v1.` prefix present.

---

### T-07 — OTEL Metrics in CloudWatch
**Method:** `aws cloudwatch list-metrics --namespace NitroEncSvc/Dev`
**Observed metrics:**

| Metric | Dimensions | Present |
|---|---|---|
| `enclave_encrypt_requests` | `OTelLib=nitro-enc-svc`, `status=success` | ✅ |
| `enclave_encrypt_requests` | `OTelLib=nitro-enc-svc`, `status=error` | ✅ |
| `enclave_encrypt_latency_ms` | `OTelLib=nitro-enc-svc`, `status=success` | ✅ |
| `enclave_dek_rotations` | `OTelLib=nitro-enc-svc` | ✅ |

**Result: PASS** — All 3 instruments flowing to CloudWatch EMF within 15 s of first request.

---

### T-08 — Load Test (Apache Benchmark — Keep-Alive)
**Tool:** `ab 2.3` (httpd-tools) from general node (i-0f59535d0c9dc6cfc)
**Endpoint:** NLB → vsock-proxy → Nitro Enclave (TLS terminating in enclave)

| Concurrency | Requests | TPS | Mean latency (ms) | Failed |
|---|---|---|---|---|
| 10  | 1,000  | **9,036**  | 1.1 | **0** |
| 50  | 5,000  | **14,420** | 3.5 | **0** |
| 100 | 10,000 | **15,030** | 6.7 | **0** |

Response: `HTTP/1.1 200 OK`, `content-length: 113` — all responses identical length, zero failures.

**Note:** At c=100, the 2-vCPU test client was saturated — the enclave is not the bottleneck.
Encryption-only path (from CloudWatch EMF) averages **0.015 ms**; practical enclave ceiling
estimated at 20,000–40,000 TPS with sufficient client workers.

**Result: PASS** — 15,030 TPS with zero failures, well above the 1,000s TPS target.

---

### T-09 — Latency (End-to-End via NLB)
10 requests measured with `curl --write-out '%{time_total}'` (new TLS per request):

| Request | Total time (s) |
|---|---|
| 1 | 0.0664 |
| 2 | 0.0539 |
| 3 | 0.0643 |
| 4 | 0.0485 |
| 5 | 0.0501 |
| 6 | 0.0510 |
| 7 | 0.0543 |
| 8 | 0.0509 |
| 9 | 0.0460 |
| 10 | 0.0499 |

**p50 ≈ 51 ms, p99 ≈ 66 ms (end-to-end including NLB + TLS handshake + vsock)**

With keep-alive (ab, c=10): **1.1 ms mean end-to-end** (no TLS handshake per request).

**Encryption-only latency** (from CloudWatch EMF — 1,880,048 samples):

| Stat | Value |
|---|---|
| Average | 0.015 ms |
| Maximum observed | 0.965 ms |

**Result: PASS** — Encryption path p99 < 1 ms; well within the < 5 ms target.

---

### T-10 — Unit Test Suite
**Command:** `cargo test --workspace`

| Crate | Tests | Result |
|---|---|---|
| `common` | 5 | PASS |
| `enclave` | 53 | PASS |
| `vsock-proxy` | 5 | PASS |
| **Total** | **63** | **PASS** |

**Result: PASS** — 63/63.

---

### T-11 — CloudWatch Dashboard
**Dashboard name:** `NitroEncSvc-Dev` (us-east-2)
**Widgets:**

| Widget | Source | Status |
|---|---|---|
| Encrypt Requests / min | `enclave_encrypt_requests` (Sum, 60s) | ✅ Data visible |
| Error Rate % | Expression over success/error counters | ✅ Data visible |
| Latency Average / Max | `enclave_encrypt_latency_ms` (Average, Maximum) | ✅ Data visible |
| DEK Rotations | `enclave_dek_rotations` (Sum, 24h) | ✅ Data visible |
| Total Requests (today) | `enclave_encrypt_requests` (Sum, 24h) | ✅ Data visible |
| Log Events / min | Logs Insights on `/nitro-enc-svc/dev/enclave` | ✅ |
| Recent Errors/Warnings | Logs Insights filter on ERROR\|WARN | ✅ |

**Root causes fixed during dashboard bring-up:**

| # | Issue | Fix |
|---|---|---|
| 1 | `awsemf` exporter auto-adds `OTelLib` dimension; initial widgets omitted it | Added `OTelLib=nitro-enc-svc` to all widget metric arrays |
| 2 | Log Insights widget used `SOURCE '...'` in query string → API rejected it | Moved log group to `logGroupNames` array; removed `SOURCE` from query |
| 3 | Latency widget used `p50/p95/p99` stats → no data (EMF exports aggregated, not raw samples) | Switched to `Average` and `Maximum` statistics |

**Result: PASS** — All widgets populated after fixes.

---

### T-12 — HTTP/1.1 Keep-Alive (ALPN + Content-Length)
**Verified via:** `curl -vsk https://<NLB>:8443/health 2>&1 | grep -E "ALPN|HTTP/"`
**Output:**
```
* ALPN: server accepted http/1.1
< HTTP/1.1 200 OK
```
**Response headers:** `content-type: application/json`, `content-length: ...` (no `transfer-encoding: chunked`)

**Root causes fixed:**

| # | Root Cause | Symptom | Fix | Commit |
|---|---|---|---|---|
| 1 | No ALPN on `rustls::ServerConfig` | Server returned HTTP/1.0; keep-alive impossible | `alpn_protocols = ["http/1.1", "h2"]` | `df908a2` |
| 2 | `h2` listed first in ALPN → server selected HTTP/2 | `ab` negotiated h2 but can't speak it; ~65% failures | Reorder to `["http/1.1", "h2"]` | `04d67e8` |
| 3 | `CompressionLayer` in Axum router | `Transfer-Encoding: chunked` (no `Content-Length`); ab counted every response as failed | Remove `CompressionLayer` | `1910780` |

**Result: PASS** — HTTP/1.1 with `content-length` on every response; zero ab failures.

---

## Summary

| Test | Description | Result |
|---|---|---|
| T-01 | Health endpoint | PASS |
| T-02 | Full PII payload encryption | PASS |
| T-03 | Missing schema header → 400 | PASS |
| T-04 | Unknown schema → 400 | PASS |
| T-05 | Determinism | PASS |
| T-06 | Encrypted value format | PASS |
| T-07 | OTEL metrics in CloudWatch | PASS |
| T-08 | Load test — 15,030 TPS, 0 failures | PASS |
| T-09 | Latency (end-to-end + encryption-only) | PASS |
| T-10 | Unit test suite 63/63 | PASS |
| T-11 | CloudWatch dashboard | PASS |
| T-12 | HTTP/1.1 keep-alive (ALPN + Content-Length) | PASS |

**12/12 PASS on image `1910780`**
