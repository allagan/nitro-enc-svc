# Action Plan — Test Finding Remediation

Derived from [test-report-2026-03-05.md](test-report-2026-03-05.md).

---

## A-1 — Fix Non-Deterministic Encryption [CRITICAL]

**Finding:** F-1 — Non-deterministic nonce generation breaks tokenization/lookup guarantee.
**File:** `crates/enclave/src/crypto/cipher.rs`
**Effort:** Small (single function change + tests)

### Design

Derive the nonce deterministically from the DEK and plaintext using HMAC-SHA256.
The nonce is the first 12 bytes of `HMAC-SHA256(key=DEK, data=plaintext)`.

Why this is safe with AES-256-GCM-SIV:
- AES-GCM-SIV is nonce-misuse resistant by design (RFC 8452). Unlike plain AES-GCM, nonce
  reuse does **not** break confidentiality or authentication — it only reveals whether two
  ciphertexts correspond to the same plaintext (which is intentional for tokenization).
- The derived nonce is a PRF of both the DEK and the plaintext. Different DEKs produce
  different nonces for the same plaintext; different plaintexts produce different nonces
  for the same DEK.

### Implementation

1. Add `hmac` and `sha2` to `crates/enclave/Cargo.toml`:
   ```toml
   hmac = "0.12"
   sha2 = "0.10"
   ```

2. Replace the `OsRng` nonce generation in `encrypt_field()`:
   ```rust
   use hmac::{Hmac, Mac};
   use sha2::Sha256;

   fn derive_nonce(dek: &[u8], plaintext: &[u8]) -> [u8; NONCE_LEN] {
       let mut mac = <Hmac<Sha256>>::new_from_slice(dek)
           .expect("HMAC accepts any key length");
       mac.update(plaintext);
       let result = mac.finalize().into_bytes();
       let mut nonce = [0u8; NONCE_LEN];
       nonce.copy_from_slice(&result[..NONCE_LEN]);
       nonce
   }
   ```
   Replace the `OsRng.fill_bytes` block with `let nonce_bytes = derive_nonce(dek, plaintext);`.

3. Remove the `OsRng` import from `encrypt_field` (it is still needed by test helpers).

4. Update the doc comment on `encrypt_field` to describe deterministic behaviour.

5. Add unit tests:
   - `encrypt_same_input_twice_matches` — same plaintext + DEK → identical ciphertext
   - `encrypt_different_plaintext_differs` — different plaintext → different ciphertext
   - `encrypt_different_dek_differs` — same plaintext, different DEK → different ciphertext

### Acceptance Criteria
- Test T-04 (determinism) passes: two consecutive encryptions of the same plaintext and
  DEK produce byte-identical output.
- All existing round-trip and tamper tests continue to pass.
- `cargo clippy` and `cargo test --workspace` both pass with zero warnings.

---

## A-2 — Fix NLB Cross-Node Routing [MEDIUM]

**Finding:** F-2 — `externalTrafficPolicy: Cluster` routes traffic to all nodes, causing cross-node
hops when the NLB hits a node without a vsock-proxy pod.
**File:** `deploy/vsock-proxy-service.yaml`
**Effort:** One-line change + re-apply

### Design

Set `externalTrafficPolicy: Local` on the `vsock-proxy-nlb` Service. This instructs the NLB
to only route to nodes where a matching pod is currently running. Kube-proxy programs the
iptables rules to drop health checks on nodes without a local pod, so the NLB removes those
nodes from its target group automatically.

The vsock-proxy pod already has `nodeSelector: aws.amazon.com/nitro-enclaves: "true"`,
ensuring it only runs on the nitro node. With `externalTrafficPolicy: Local`, the NLB will
only target that node, eliminating cross-node hops entirely.

**Side effect:** The client's original source IP is preserved (no SNAT), which is useful for
access logging.

### Implementation

In `deploy/vsock-proxy-service.yaml`, under `spec:`:
```yaml
spec:
  type: LoadBalancer
  externalTrafficPolicy: Local   # ← add this line
  selector:
    app: vsock-proxy
  ports:
    - name: https
      port: 8443
      targetPort: 8443
      protocol: TCP
```

Apply with `kubectl apply -f deploy/vsock-proxy-service.yaml`.

The NLB target group will update within ~1 minute to remove the general-purpose node.

### Acceptance Criteria
- `aws elbv2 describe-target-health` shows only the nitro node (10.0.52.83) as healthy.
- Zero timeout failures across 20 consecutive requests from bastion / test-client.

---

## A-3 — Suppress NLB Health Check Noise in CloudWatch [LOW]

**Finding:** F-3 — NLB TCP health checks flood CloudWatch with `WARN TLS handshake failed`
at ~6 messages/minute.
**Files:** `deploy/vsock-proxy-service.yaml`, `deploy/otel-collector.yaml`
**Effort:** Small

### Option 1 — HTTP Health Check (Recommended)

Add an annotation to configure the NLB target group to use an HTTP health check against
`/health` rather than a raw TCP probe. The vsock-proxy handles this at the TCP level by
forwarding the request to the enclave, which responds with `200 {"status":"ok",...}`.

Add to the Service annotations in `deploy/vsock-proxy-service.yaml`:
```yaml
annotations:
  service.beta.kubernetes.io/aws-load-balancer-healthcheck-protocol: "HTTPS"
  service.beta.kubernetes.io/aws-load-balancer-healthcheck-path: "/health"
  service.beta.kubernetes.io/aws-load-balancer-healthcheck-interval: "30"
  service.beta.kubernetes.io/aws-load-balancer-healthcheck-healthy-threshold: "2"
  service.beta.kubernetes.io/aws-load-balancer-healthcheck-unhealthy-threshold: "2"
```

Note: the in-tree NLB controller has limited annotation support for health checks. If these
annotations are not honoured, use the AWS Load Balancer Controller (see Option 3) or
apply Option 2 instead.

### Option 2 — OTEL Collector Log Filter

Add a filter processor to the OTEL Collector pipeline to drop log records whose message
matches the health-check warning pattern before they reach CloudWatch:

In `deploy/otel-collector.yaml`, add to `processors:`:
```yaml
filter/drop_nlb_noise:
  logs:
    exclude:
      match_type: regexp
      bodies:
        - '"message":"TLS handshake failed"'
```

Add `filter/drop_nlb_noise` before `batch` in the logs pipeline:
```yaml
pipelines:
  logs:
    receivers: [tcplog]
    processors: [resource, filter/drop_nlb_noise, batch]
    exporters: [awscloudwatchlogs, debug]
```

This keeps the enclave healthy (the NLB probe succeeds at the TCP level) while removing
the noise from CloudWatch.

### Option 3 — AWS Load Balancer Controller (Future)

Install the AWS Load Balancer Controller addon to the EKS cluster. It provides full
`TargetGroupBinding` control including fine-grained health check configuration and
avoids the limitations of the in-tree NLB controller.

### Acceptance Criteria
- CloudWatch `/nitro-enc-svc/dev/enclave` stream contains no more than 1 `TLS handshake
  failed` entry per 5-minute window during steady-state (if Option 1 or 3); or zero entries
  (if Option 2).
- NLB target group continues to report both targets as healthy.

---

## Priority and Sequencing

| # | Action | Priority | Blocking? |
|---|---|---|---|
| A-1 | Fix non-deterministic encryption | CRITICAL | Yes — tokenization doesn't work without this |
| A-2 | Fix NLB cross-node routing | MEDIUM | No — intermittent, not a correctness issue |
| A-3 | Suppress health check noise | LOW | No — cosmetic / operational quality |

**Recommended order:** A-1 → A-2 → A-3.

A-1 requires a code change → new EIF build → pipeline approval → DaemonSet rollout.
A-2 and A-3 are config-only changes deployable without a new EIF build.
