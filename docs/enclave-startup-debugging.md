# Nitro Enclave Startup Debugging — Session Log

## Overview

This document records the debugging session that brought the `nitro-enc-svc` Nitro Enclave
service from a state of crashing on every launch to a fully running, health-checked service.

The enclave is a Rust binary (`crates/enclave`) that:
1. Runs inside an AWS Nitro Enclave as PID 1
2. Fetches and decrypts a Data Encryption Key (DEK) from Secrets Manager + KMS via vsock
3. Loads OpenAPI schemas from S3 via vsock
4. Serves a TLS HTTPS API on port 443 for field-level PII encryption

All communication with AWS services goes through `vsock-proxy` instances running on the parent
EC2 host. The enclave has no direct network access.

---

## Starting State

The pipeline had successfully built commit `8d4c881`. The DaemonSet pod would start the runner
container, which launched the enclave via `nitro-cli run-enclave`, but the enclave crashed on
every attempt. The runner entered `CrashLoopBackOff`.

---

## Bug 1 — Rustls CryptoProvider Panic

**Commit**: `45a9b62`

### Symptom

The enclave (run in `--debug-mode` to view console output) booted the kernel successfully, started
the entrypoint, and then the Rust binary immediately panicked:

```
thread 'main' panicked at 'rustls-0.23.37/src/crypto/mod.rs:249:14:
Could not automatically determine the process-level CryptoProvider from Rustls crate features.
```

### Root Cause

The enclave binary has two transitive dependencies that each pull in rustls 0.23.x with different
crypto backends:

- `hyper-rustls` → compiled with the **ring** feature
- `opentelemetry-otlp` (via tonic) → compiled with the **aws-lc-rs** feature

When rustls 0.23+ detects more than one crypto provider compiled in, it refuses to pick one
automatically and panics unless `CryptoProvider::install_default()` has been called explicitly
before any TLS operation.

### Fix

Added the following as the very first statement in `main()`, before anything else:

```rust
// crates/enclave/src/main.rs
rustls::crypto::aws_lc_rs::default_provider()
    .install_default()
    .expect("failed to install rustls CryptoProvider");
```

**Must come before** `init_telemetry()`, `AwsClients::init()`, and any other code that touches TLS.

---

## Bug 2 — IMDS Credential Failure (socat without vsock support)

**Commit**: `c0bba2f`

### Symptom

After the rustls fix, the enclave booted and ran until it tried to fetch the DEK from Secrets
Manager. The error was:

```
Error: failed to fetch DEK from Secrets Manager
Caused by:
    0: dispatch failure
    1: other
    2: the credential provider was not enabled
    3: no providers in chain provided credentials
```

### Root Cause

The AWS SDK inside the enclave needs EC2 instance credentials, which it fetches from IMDS
(`http://169.254.169.254`). Since the enclave has no network, the SDK is configured with
`AWS_EC2_METADATA_SERVICE_ENDPOINT=http://127.0.0.1:8004`, and the entrypoint script was
supposed to start a `socat` bridge on that port:

```sh
socat TCP4-LISTEN:8004,bind=127.0.0.1,fork,reuseaddr VSOCK-CONNECT:${VSOCK_PROXY_CID}:8004 &
```

The problem: **socat on Amazon Linux 2023 does not have vsock support compiled in**. The
`VSOCK-CONNECT` address type was not recognized, so socat exited immediately (silently, because
it was a background process). Nothing was listening on `127.0.0.1:8004`, so IMDS requests
failed with "connection refused" and no credentials were available.

### Fix

Replaced the socat bridge with a pure-Rust implementation in `main.rs` using `tokio-vsock`
(already a workspace dependency):

```rust
// crates/enclave/src/main.rs
async fn start_imds_bridge(parent_cid: u32) -> Result<()> {
    const IMDS_LOCAL_PORT: u16 = 8004;
    const IMDS_VSOCK_PORT: u32 = 8004;

    let listener = TcpListener::bind(("127.0.0.1", IMDS_LOCAL_PORT))
        .await
        .context("failed to bind IMDS bridge on 127.0.0.1:8004")?;

    eprintln!(
        "INFO: IMDS bridge listening on 127.0.0.1:{IMDS_LOCAL_PORT} \
         -> vsock({parent_cid},{IMDS_VSOCK_PORT})"
    );

    tokio::spawn(async move {
        loop {
            let (tcp, _peer) = match listener.accept().await {
                Ok(c) => c,
                Err(e) => { eprintln!("WARN: IMDS bridge accept error: {e}"); continue; }
            };
            tokio::spawn(async move {
                let vsock = match VsockStream::connect(
                    VsockAddr::new(parent_cid, IMDS_VSOCK_PORT)
                ).await {
                    Ok(s) => s,
                    Err(e) => { eprintln!("WARN: IMDS bridge vsock connect error: {e}"); return; }
                };
                let (mut tr, mut tw) = tokio::io::split(tcp);
                let (mut vr, mut vw) = tokio::io::split(vsock);
                tokio::select! {
                    _ = tokio::io::copy(&mut tr, &mut vw) => {}
                    _ = tokio::io::copy(&mut vr, &mut tw) => {}
                }
            });
        }
    });

    Ok(())
}
```

Called in `main()` **before** `AwsClients::init()`:

```rust
start_imds_bridge(cfg.vsock_proxy_cid).await?;
```

Also simplified `scripts/enclave-entrypoint.sh` by removing all socat-related code, and removed
`socat` from the `dnf install` in `Dockerfile.enclave`.

**Note on CID**: `VSOCK_PROXY_CID=3` is baked into the EIF at build time (set in the CodeBuild
project). In AWS Nitro Enclaves, the parent EC2 instance always has CID 3 as seen from inside
the enclave (`VMADDR_CID_HOST`).

---

## Bug 3 — Loopback Interface Not Up in Enclave

**Commit**: `d7c2a54`

### Symptom

After the IMDS bridge change, the enclave began crashing even faster — in under 200ms, before
`nitro-cli console` could even connect. The error in the runner log was:

```
[ E44 ] Enclave console connection failure.
Failed to connect to the console: ETIMEDOUT
```

The `nitro_enclaves.log` showed a `HANG-UP` event only ~207ms after the enclave ID was assigned.
No kernel messages were visible on the console at all.

### Root Cause

The Nitro Enclave kernel **does not bring up the loopback interface (`lo`) automatically**. There
is no init system (systemd, OpenRC, etc.) inside the enclave — the Docker image's `ENTRYPOINT`
becomes PID 1 directly.

`start_imds_bridge()` calls:

```rust
let listener = TcpListener::bind(("127.0.0.1", IMDS_LOCAL_PORT)).await?;
```

With `lo` in the DOWN state, `bind(2)` returns `EADDRNOTAVAIL` (error 99: "Cannot assign
requested address"). The `?` operator propagates this as `Err`, `main()` returns, PID 1 exits,
and the entire enclave VM terminates — in under 200ms, before the serial console port is
even reachable by `nitro-cli console`.

**Why this didn't fail in the socat era**: socat was a background process (`&`), so its failure
was silent and the entrypoint continued. The binary never tried to bind to `127.0.0.1`, so the
loopback state was irrelevant.

### Diagnostic Method

Deployed a temporary `--debug-mode` command override in the DaemonSet:

```yaml
command: ["/bin/bash", "-c"]
args:
  - |
    RUN_OUTPUT=$(nitro-cli run-enclave \
      --eif-path "${EIF_PATH:-/enclave/nitro-enc-svc.eif}" \
      --memory "${ENCLAVE_MEMORY_MB}" --cpu-count "${ENCLAVE_CPU_COUNT}" \
      --enclave-cid "${ENCLAVE_CID}" --debug-mode 2>&1)
    ENCLAVE_ID=$(echo "${RUN_OUTPUT}" | awk '/^\{/,0' | jq -r '.EnclaveID // empty')
    cleanup() { nitro-cli terminate-enclave --enclave-id "${ENCLAVE_ID}" 2>/dev/null || true; }
    trap cleanup EXIT TERM INT
    nitro-cli console --enclave-id "${ENCLAVE_ID}"
```

`kubectl logs` then streams the enclave's serial console output. This was used for all subsequent
debugging steps in this session.

### Fix

Two changes:

**1. `scripts/enclave-entrypoint.sh`** — bring up loopback before exec:

```sh
# The Nitro Enclave kernel does not bring up the loopback interface
# automatically.  The IMDS vsock bridge in the enclave binary binds to
# 127.0.0.1:8004, which requires lo to be UP.  Without this, bind(2) fails
# with EADDRNOTAVAIL and PID 1 exits before the console can connect.
ip link set lo up 2>/dev/null || true

echo "INFO: exec nitro-enc-svc"
exec /usr/local/bin/enclave "$@"
```

**2. `Dockerfile.enclave` runtime stage** — install `iproute` to get the `ip` command:

```dockerfile
RUN dnf install -y ca-certificates openssl iproute \
    && dnf clean all \
    ...
```

The AL2023 minimal container base image does not include `iproute` by default.

---

## Bug 4 — S3 Virtual-Hosted-Style Hostname Routing

**Commit**: `442acc7`

### Symptom

After the loopback fix, the enclave booted fully and the debug console showed:

```
INFO: IMDS bridge listening on 127.0.0.1:8004 -> vsock(3,8004)
{"message":"nitro-enc-svc starting","version":"0.1.0","tls_port":443}
{"message":"DEK fetched and stored successfully"}
Error: failed to list S3 objects for schemas
Caused by:
    0: dispatch failure
    1: io error
    2: client error (Connect)
    3: invalid peer certificate: certificate not valid for name
       "dev-nitro-enc-svc-schemas-394582308905.s3.us-east-2.amazonaws.com";
       certificate is only valid for DnsName("kms.us-east-2.amazonaws.com"), ...
```

DEK fetch succeeded (Secrets Manager + KMS working), but S3 list-objects got a **KMS TLS
certificate** when connecting.

### Root Cause

The vsock connector (`aws/vsock_connector.rs`) maps AWS service hostnames to vsock ports:

```rust
fn vsock_port(host: &str, base_port: u32) -> u32 {
    if host.starts_with("kms.") {
        base_port + 1  // 8001 → KMS vsock-proxy
    } else if host.starts_with("secretsmanager.") {
        base_port + 2  // 8002 → Secrets Manager vsock-proxy
    } else if host.starts_with("s3.") || host == "s3.amazonaws.com" {
        base_port + 3  // 8003 → S3 vsock-proxy
    } else {
        base_port + 1  // fallback to KMS port  ← BUG
    }
}
```

The AWS SDK has used **virtual-hosted-style S3 URLs by default since 2020**:

```
<bucket-name>.s3.<region>.amazonaws.com
```

For the bucket `dev-nitro-enc-svc-schemas-394582308905`, the SDK connects to:

```
dev-nitro-enc-svc-schemas-394582308905.s3.us-east-2.amazonaws.com
```

This hostname starts with `dev-nitro-enc-svc-schemas-...`, not `s3.`. So `vsock_port()` fell
through to the `else` branch (KMS port 8001). The KMS vsock-proxy at port 8001 forwarded to
`kms.us-east-2.amazonaws.com`, which returned its own TLS certificate — causing the "certificate
not valid for name" error.

### Fix

Added `.contains(".s3.")` to the S3 branch:

```rust
} else if host.starts_with("s3.") || host == "s3.amazonaws.com" || host.contains(".s3.") {
    // Match both path-style (s3.<region>.amazonaws.com) and virtual-hosted-style
    // (<bucket>.s3.<region>.amazonaws.com) S3 endpoints.
    base_port + 3
```

Added a test for virtual-hosted-style:

```rust
#[test]
fn port_mapping_s3() {
    assert_eq!(vsock_port("s3.us-east-2.amazonaws.com", 8000), 8003);
    assert_eq!(vsock_port("s3.amazonaws.com", 8000), 8003);
    // Virtual-hosted-style: <bucket>.s3.<region>.amazonaws.com
    assert_eq!(vsock_port("my-bucket.s3.us-east-2.amazonaws.com", 8000), 8003);
}
```

---

## Bug 5 — TLS Certificate at /run/acm Not in EIF

**Commits**: `f594780` (Dockerfile fix), `43021e3` (buildspec fix)

### Symptom

After the S3 fix, the debug console showed:

```
{"message":"DEK fetched and stored successfully"}
{"message":"loaded schema from S3","schema":"payments-v1"}
{"message":"schema cache refreshed","count":1}
Error: failed to read TLS cert: /run/acm/tls.crt
Caused by:
    No such file or directory (os error 2)
```

DEK fetch and schema load both succeeded, but the TLS certificate was missing.

### Root Cause

The `Dockerfile.enclave` runtime stage generated a self-signed dev certificate at build time:

```dockerfile
RUN dnf install -y ca-certificates openssl iproute \
    && dnf clean all \
    && mkdir -p /run/acm \
    && openssl req -x509 -newkey rsa:2048 -sha256 \
         -keyout /run/acm/tls.key \
         -out /run/acm/tls.crt \
         -days 3650 -nodes \
         -subj '/CN=nitro-enc-svc.local' \
    && chmod 400 /run/acm/tls.key /run/acm/tls.crt
```

On Amazon Linux 2023, **`/run` is a `tmpfs` filesystem** — an in-memory filesystem that is
freshly mounted at OS/kernel boot. Files written to `/run` during a `docker build` layer are
committed to the image layer on disk, but when the Nitro Enclave kernel boots and mounts a fresh
`tmpfs` at `/run`, those files are discarded. The EIF contains the file in its initramfs, but
the kernel overwrites that directory with an empty tmpfs before PID 1 starts.

The result: `/run/acm/tls.crt` does not exist when the enclave binary tries to read it.

### Fix — Part 1: Dockerfile.enclave (commit f594780)

Changed the certificate generation directory from `/run/acm/` to `/etc/acm/`:

```dockerfile
RUN dnf install -y ca-certificates openssl iproute \
    && dnf clean all \
    && mkdir -p /etc/acm \
    && openssl req -x509 -newkey rsa:2048 -sha256 \
         -keyout /etc/acm/tls.key \
         -out /etc/acm/tls.crt \
         -days 3650 -nodes \
         -subj '/CN=nitro-enc-svc.local' \
    && chmod 400 /etc/acm/tls.key /etc/acm/tls.crt
```

Also updated the default build args:

```dockerfile
ARG TLS_CERT_PATH=/etc/acm/tls.crt
ARG TLS_KEY_PATH=/etc/acm/tls.key
```

### Fix — Part 2: buildspec.yml (commit 43021e3) — the hidden override

After deploying `f594780`, the error persisted — the binary was still reading
`/run/acm/tls.crt`.

The `buildspec.yml` passes all build args explicitly to `docker build`:

```yaml
--build-arg TLS_CERT_PATH="${TLS_CERT_PATH:-/run/acm/tls.crt}" \
--build-arg TLS_KEY_PATH="${TLS_KEY_PATH:-/run/acm/tls.key}" \
```

The shell syntax `${TLS_CERT_PATH:-/run/acm/tls.crt}` means: "use the environment variable
`TLS_CERT_PATH` if set, otherwise use `/run/acm/tls.crt`". Since `TLS_CERT_PATH` is not
configured as a CodeBuild project variable, the old hardcoded default was always used —
completely overriding the Dockerfile default.

Fixed by updating the buildspec fallback values:

```yaml
--build-arg TLS_CERT_PATH="${TLS_CERT_PATH:-/etc/acm/tls.crt}" \
--build-arg TLS_KEY_PATH="${TLS_KEY_PATH:-/etc/acm/tls.key}" \
```

**Key lesson**: When a buildspec passes `--build-arg KEY="${KEY:-default}"` to Docker, the
buildspec default takes precedence over the Dockerfile `ARG KEY=default`. Both locations must
be updated together.

---

## Final State

After commit `43021e3`, the enclave started successfully and has been stable:

```
INFO: Starting Nitro Enclave
INFO:   EIF path:              /enclave/nitro-enc-svc.eif
INFO:   Memory (MB):           1024
INFO:   CPU count:             2
INFO:   CID:                   16
INFO: Enclave launched. EnclaveID=i-0ee717e3059a28bb5-enc19cb1b17cb902f3
INFO: Entering health monitoring loop (interval=10s)
INFO: Enclave i-0ee717e3059a28bb5-enc19cb1b17cb902f3 is RUNNING
INFO: Enclave i-0ee717e3059a28bb5-enc19cb1b17cb902f3 is RUNNING
...
```

Console output (debug mode) confirmed the full startup sequence:

```
INFO: exec nitro-enc-svc
INFO: IMDS bridge listening on 127.0.0.1:8004 -> vsock(3,8004)
{"message":"nitro-enc-svc starting","version":"0.1.0","tls_port":443}
{"message":"DEK fetched and stored successfully"}
{"message":"loaded schema from S3","schema":"payments-v1","key":"schemas/payments-v1.yaml"}
{"message":"schema cache refreshed","count":1}
{"message":"listening (TLS)","addr":"0.0.0.0:443"}
```

**DaemonSet**: `1/1 Running`, 0 restarts.

---

## Summary of All Changes

| File | Change |
|------|--------|
| `crates/enclave/src/main.rs` | Add `rustls::crypto::aws_lc_rs::default_provider().install_default()` as first line of `main()` |
| `crates/enclave/src/main.rs` | Add `start_imds_bridge()` async function; call before `AwsClients::init()` |
| `crates/enclave/src/aws/vsock_connector.rs` | Add `\|\| host.contains(".s3.")` to S3 branch of `vsock_port()` |
| `scripts/enclave-entrypoint.sh` | Remove socat bridge; add `ip link set lo up 2>/dev/null \|\| true` |
| `Dockerfile.enclave` (runtime stage) | Remove `socat` from `dnf install`; add `iproute`; move cert dir from `/run/acm/` to `/etc/acm/`; update `ARG TLS_*_PATH` defaults |
| `buildspec.yml` | Update `--build-arg TLS_CERT_PATH` and `TLS_KEY_PATH` fallback defaults from `/run/acm/` to `/etc/acm/` |
| `deploy/daemonset-enclave.yaml` | Update image tag through each build iteration |

---

## Debugging Methodology

### Debug Mode DaemonSet Override

The most important diagnostic tool was running the enclave in `--debug-mode` by patching the
DaemonSet command. `--debug-mode` exposes the enclave's serial console, and `nitro-cli console`
streams it to stdout, which `kubectl logs` can capture:

```yaml
command: ["/bin/bash", "-c"]
args:
  - |
    RUN_OUTPUT=$(nitro-cli run-enclave \
      --eif-path "${EIF_PATH:-/enclave/nitro-enc-svc.eif}" \
      --memory "${ENCLAVE_MEMORY_MB}" --cpu-count "${ENCLAVE_CPU_COUNT}" \
      --enclave-cid "${ENCLAVE_CID}" --debug-mode 2>&1)
    ENCLAVE_ID=$(echo "${RUN_OUTPUT}" | awk '/^\{/,0' | jq -r '.EnclaveID // empty')
    cleanup() { nitro-cli terminate-enclave --enclave-id "${ENCLAVE_ID}" 2>/dev/null || true; }
    trap cleanup EXIT TERM INT
    nitro-cli console --enclave-id "${ENCLAVE_ID}"
```

Remove the `command:` and `args:` override after debugging to restore normal operation.

### Reading Build Summary

After each CodeBuild run, get the new image tag and PCR0 measurement:

```bash
# Find the latest artifact ZIP
ARTIFACT=$(aws s3 ls s3://dev-nitro-enc-svc-artifacts-394582308905/nitro-enc-svc-dev/build_outp/ \
  | sort | tail -1 | awk '{print $NF}')

aws s3 cp "s3://dev-nitro-enc-svc-artifacts-394582308905/nitro-enc-svc-dev/build_outp/${ARTIFACT}" \
  /tmp/build.zip

unzip -p /tmp/build.zip enclave/build-summary.json
```

### Approving the Pipeline Gate

```bash
# Get the approval token
TOKEN=$(aws codepipeline get-pipeline-state --name nitro-enc-svc-dev \
  --query 'stageStates[?stageName==`Approve`].actionStates[0].latestExecution.token' \
  --output text)

# Approve
aws codepipeline put-approval-result \
  --pipeline-name nitro-enc-svc-dev \
  --stage-name Approve \
  --action-name ReviewPCR0 \
  --result summary="PCR0 <value> approved",status=Approved \
  --token "$TOKEN"
```

### Checking vsock-proxy Processes

The vsock-proxy instances on the parent EC2 are bare processes that do not survive node reboots.
Verify they are running via SSM:

```bash
aws ssm send-command \
  --instance-ids i-0ee717e3059a28bb5 \
  --document-name AWS-RunShellScript \
  --parameters 'commands=["ps aux | grep vsock-proxy | grep -v grep"]'
```

Expected output: four `vsock-proxy` processes on ports 8001–8004.

---

## Architecture Reference

```
Enclave (PID 1: enclave-entrypoint.sh)
  └─ ip link set lo up            ← Bug 3 fix: must be explicit
  └─ exec /usr/local/bin/enclave

enclave binary (main.rs):
  1. rustls::install_default()    ← Bug 1 fix: before any TLS
  2. Config::from_env()
  3. start_imds_bridge(CID=3)     ← Bug 2 fix: replaces socat
       TCP 127.0.0.1:8004 → vsock(3, 8004) → vsock-proxy → 169.254.169.254:80
  4. init_telemetry(OTLP)
  5. AwsClients::init(CID=3, base=8000)
       vsock_port():
         kms.*          → 8001 → vsock-proxy → kms.us-east-2.amazonaws.com:443
         secretsmanager.* → 8002 → vsock-proxy → secretsmanager...amazonaws.com:443
         s3.* or *.s3.* → 8003 → vsock-proxy → s3.amazonaws.com:443  ← Bug 4 fix
         127.0.0.1      → TCP (IMDS bridge above)
  6. dek::fetch_and_store()        ← Secrets Manager → KMS → in-memory DEK
  7. schema::load_all()            ← S3 list + fetch → in-memory schema cache
  8. spawn rotation_task + refresh_task
  9. read /etc/acm/tls.crt        ← Bug 5 fix: not /run/acm/
     read /etc/acm/tls.key
  10. TLS server loop on :443
```

**Vsock CID reference**:
- `ENCLAVE_CID=16`: the enclave's CID as seen **from the parent EC2** (used by vsock-proxy sidecars to connect to the enclave)
- `VSOCK_PROXY_CID=3`: the parent EC2's CID as seen **from inside the enclave** (used by the enclave to connect outbound)

---

## Bug 6 — TLS Server Not Reachable via Vsock (ECONNRESET)

**Commit**: `259ea13`

### Symptom

After the enclave reached a stable RUNNING state (health checks passing, DEK loaded, schemas
loaded), the vsock-proxy sidecar (`deploy/vsock-proxy-test.yaml`) could not reach it:

```
curl: exit code 35 (SSL connect error)
vsock-proxy log: "Connection reset by peer (os error 104)"
```

Confirmed from the parent EC2 host via SSM Python test:
```python
s = socket.socket(socket.AF_VSOCK, socket.SOCK_STREAM)
rc = s.connect_ex((16, 443))  # ECONNRESET (104) even from host
```

### Root Cause

The enclave TLS server in `main.rs` was listening on a **TCP socket**
(`tokio::net::TcpListener::bind("0.0.0.0:443")`).

**Nitro Enclaves have no external network interface.** TCP sockets inside the enclave are
isolated in the enclave's private network namespace. The parent EC2 — and any vsock-proxy
sidecar — can only reach the enclave via **AF_VSOCK**. TCP socket ports are not automatically
bridged to vsock connections; the enclave binary must explicitly listen on `AF_VSOCK` for
vsock-initiated connections to succeed.

The IMDS bridge in `start_imds_bridge()` worked because it is the **reverse direction**
(enclave initiates outbound vsock connections to the parent). Incoming connections from the
parent require the enclave to listen on vsock.

### Fix

Changed the TLS accept loop from `TcpListener` to `VsockListener`:

```rust
// Before: TCP (not reachable from parent via vsock)
let listener = tokio::net::TcpListener::bind("0.0.0.0:443").await?;

// After: vsock (reachable from parent via vsock(ENCLAVE_CID=16, port=443))
let mut listener = VsockListener::bind(VsockAddr::new(VMADDR_CID_ANY, cfg.tls_port as u32))
    .context("failed to bind vsock TLS listener")?;
```

`VMADDR_CID_ANY` (0xFFFFFFFF) accepts connections from any peer CID. `VsockStream` implements
`AsyncRead + AsyncWrite + Unpin`, so `TlsAcceptor::accept()` and `hyper_util::TokioIo` work
unchanged.

**Files changed**: `crates/enclave/src/main.rs`

### Verified Working

```
$ kubectl exec -n nitro-enc-svc vsock-proxy-test -c test-client -- \
    curl -sk https://127.0.0.1:8443/health
{"status":"ok","dek_ready":true,"schemas_loaded":1}

$ kubectl exec -n nitro-enc-svc vsock-proxy-test -c test-client -- \
    curl -sk -X POST https://127.0.0.1:8443/encrypt \
    -H "Content-Type: application/json" \
    -H "X-Schema-Name: payments-v1" \
    -d '{"payload":{"merchant_id":"acme","card_number":"4111111111111111",...}}'
{
  "payload": {
    "card_number":      "v1.OJzZnh9kG4Eq-CKG.iTBA2KWRfzBP...",
    "card_holder_name": "v1.6LUYHN5BTRqbdAUD.fqntQwa43Kx...",
    "billing_address":  {"street": "v1...", "zip": "v1...", "city": "Springfield"},
    "merchant_id":      "acme",
    "amount_cents":     9999
  }
}
```

PII fields are encrypted; non-PII fields pass through unmodified. **End-to-end flow verified.**

**Vsock CID reference**:
- `ENCLAVE_CID=16`: the enclave's CID as seen **from the parent EC2** (used by vsock-proxy sidecars to connect to the enclave)
- `VSOCK_PROXY_CID=3`: the parent EC2's CID as seen **from inside the enclave** (used by the enclave to connect outbound)
