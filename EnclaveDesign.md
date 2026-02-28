# Enclave Build and Deployment Design

## Overview

This document describes how to build an EIF (Enclave Image File) from the `nitro-enc-svc`
Rust service and deploy it into a Nitro Enclave on an Amazon EKS node.

---

## Build Pipeline

```
Source (Git)
    │
    ▼
AWS CodeBuild (buildspec.yml)
    │
    ├─► cargo test / clippy / fmt
    │
    ├─► docker build -f Dockerfile.enclave   ← enclave binary + baked-in config ENV vars
    │         │
    │         ▼
    │   nitro-cli build-enclave
    │         │
    │         ├─► nitro-enc-svc.eif          ← Enclave Image File
    │         └─► pcr-values.json            ← PCR0/1/2 for KMS key policy
    │
    ├─► docker build -f Dockerfile.runner    ← AL2023 + nitro-cli + EIF baked in
    │         │
    │         └─► push → ECR ($ECR_REPO_RUNNER:$IMAGE_TAG)
    │
    └─► docker build -f Dockerfile.vsock-proxy
              │
              └─► push → ECR ($ECR_REPO_PROXY:$IMAGE_TAG)
```

---

## Key Design Constraint: Environment Variables Must Be Baked into the EIF

`nitro-cli run-enclave` **cannot** inject environment variables from the host into the
enclave process. The enclave boots in a fully isolated VM — the only way to pass
configuration is to bake it into the Docker image that is converted to an EIF.

`Dockerfile.enclave` uses Docker `ARG` → `ENV` pairs for all 12 config values that
`Config::from_env()` reads (`crates/enclave/src/config.rs`). Each per-environment
CodeBuild project sets these as build-time environment variables.

**Consequence**: changing any config value (or the Rust code) requires:
1. Rebuilding the EIF → new PCR values
2. Updating the KMS key policy with the new PCR0 value
3. Deploying the new runner image

---

## Files

| File | Purpose |
|---|---|
| `Dockerfile.enclave` | Builds the enclave OCI image. Input to `nitro-cli build-enclave`. NOT pushed to ECR. |
| `Dockerfile.vsock-proxy` | Builds the vsock-proxy sidecar container image. Pushed to ECR. |
| `Dockerfile.runner` | DaemonSet runner image: AL2023 + nitro-cli + baked-in EIF. Pushed to ECR. |
| `scripts/run-enclave.sh` | Runner entrypoint: launches enclave, streams logs, health-monitors. |
| `buildspec.yml` | AWS CodeBuild pipeline spec. |
| `deploy/daemonset-enclave.yaml` | Kubernetes DaemonSet for the runner (one per EKS node). |
| `deploy/pod-example.yaml` | Example Pod spec with vsock-proxy sidecar. |

---

## Dockerfile.enclave — Enclave OCI Image

### What it does
Multi-stage build:
- **Stage 1 (builder)**: Amazon Linux 2023 + Rust stable + cmake/gcc (needed by `aws-lc-sys`).
  Builds `cargo build --release --locked -p enclave`. Uses a dependency-caching stub layer
  so Docker layer cache is preserved when only source files change.
- **Stage 2 (runtime)**: Minimal AL2023 base. Copies the stripped `enclave` binary.
  Declares all 12 config env vars as `ARG`/`ENV`. Runs as non-root `enclave-svc`.
  `ENTRYPOINT ["/usr/local/bin/enclave"]` — boots as PID 1 in the Nitro VM kernel.

### Required build args (must be supplied via `--build-arg` or CodeBuild env vars)

| Build Arg | Description |
|---|---|
| `SECRET_ARN` | Secrets Manager ARN of the envelope-encrypted DEK |
| `KMS_KEY_ID` | KMS key ID used to decrypt the DEK |
| `S3_BUCKET` | S3 bucket containing OpenAPI schema files |
| `VSOCK_PROXY_CID` | Vsock CID of the parent EC2 `aws-vsock-proxy` |
| `OTEL_EXPORTER_OTLP_ENDPOINT` | OTLP endpoint (vsock address of OTEL Collector) |

### Optional build args (defaults match `config.rs`)

| Build Arg | Default |
|---|---|
| `S3_PREFIX` | `schemas/` |
| `SCHEMA_HEADER_NAME` | `X-Schema-Name` |
| `DEK_ROTATION_INTERVAL_SECS` | `3600` |
| `SCHEMA_REFRESH_INTERVAL_SECS` | `300` |
| `VSOCK_PROXY_PORT` | `8000` |
| `TLS_PORT` | `443` |
| `LOG_LEVEL` | `info` |

### Local build example (for testing)
```bash
docker build -f Dockerfile.enclave \
  --build-arg SECRET_ARN=arn:aws:secretsmanager:us-east-1:123456789012:secret:nitro/dek \
  --build-arg KMS_KEY_ID=arn:aws:kms:us-east-1:123456789012:key/00000000-... \
  --build-arg S3_BUCKET=my-schema-bucket \
  --build-arg VSOCK_PROXY_CID=3 \
  --build-arg OTEL_EXPORTER_OTLP_ENDPOINT=http://127.0.0.1:4317 \
  -t nitro-enc-svc-enclave:local .
```

---

## Dockerfile.runner — DaemonSet Runner Image

### What it does
- Based on AL2023 with `aws-nitro-enclaves-cli` installed.
- Copies the pre-built EIF (`enclave/nitro-enc-svc.eif`) into `/enclave/`.
- Copies `scripts/run-enclave.sh` as the container entrypoint.
- Runs as root (required by `nitro-cli`).

### How the EIF ends up in this image
In CodeBuild, `nitro-cli build-enclave` writes the EIF to `enclave/nitro-enc-svc.eif`
in the build workspace. `Dockerfile.runner` then `COPY`s it in. This bakes the EIF into
the runner image, so a single image pull is sufficient at pod startup.

---

## scripts/run-enclave.sh — Runner Entrypoint

### Startup sequence
1. Validate required env vars: `ENCLAVE_MEMORY_MB`, `ENCLAVE_CPU_COUNT`, `ENCLAVE_CID`.
2. Call `nitro-cli run-enclave --eif-path /enclave/nitro-enc-svc.eif \`
   `--memory $ENCLAVE_MEMORY_MB --cpu-count $ENCLAVE_CPU_COUNT --enclave-cid $ENCLAVE_CID`.
3. Extract `EnclaveID` from the JSON output.
4. Start `nitro-cli console --enclave-id $ENCLAVE_ID &` in background so enclave
   application logs are visible via `kubectl logs <pod>`.
5. Register a `trap` to call `nitro-cli terminate-enclave` on `EXIT/TERM/INT`.
6. Enter a polling loop: every `$HEALTH_CHECK_INTERVAL` seconds, call
   `nitro-cli describe-enclaves` and `exit 1` if the enclave state is not `RUNNING`.
   Kubernetes detects the non-zero exit and restarts the pod, which relaunches the enclave.

### Runner environment variables (set in the DaemonSet, not baked into EIF)

| Variable | Default | Description |
|---|---|---|
| `ENCLAVE_MEMORY_MB` | — | Memory (MB) allocated to the enclave via the Nitro allocator |
| `ENCLAVE_CPU_COUNT` | — | vCPUs allocated to the enclave |
| `ENCLAVE_CID` | — | Vsock CID for this enclave. **Must match `VSOCK_PROXY_CID` baked into the EIF** |
| `EIF_PATH` | `/enclave/nitro-enc-svc.eif` | Override for non-standard EIF locations |
| `HEALTH_CHECK_INTERVAL` | `10` | Seconds between health checks |

---

## buildspec.yml — AWS CodeBuild

### Prerequisites
- CodeBuild project must have **privileged mode enabled** (for Docker).
- Build environment: Amazon Linux 2023 standard image.
- IAM role must have: `ecr:GetAuthorizationToken`, `ecr:BatchCheckLayerAvailability`,
  `ecr:InitiateLayerUpload`, `ecr:UploadLayerPart`, `ecr:CompleteLayerUpload`,
  `ecr:PutImage`, `ecr:CreateRepository`, `ecr:DescribeRepositories`.

### CodeBuild environment variables

Set these on the CodeBuild project (not in `buildspec.yml` directly, since some contain
sensitive values):

**Infrastructure:**
```
ECR_REGISTRY       = 123456789012.dkr.ecr.us-east-1.amazonaws.com
ECR_REPO_ENCLAVE   = nitro-enc-svc/enclave       # local image only, not actually pushed
ECR_REPO_PROXY     = nitro-enc-svc/vsock-proxy
ECR_REPO_RUNNER    = nitro-enc-svc/runner
AWS_DEFAULT_REGION = us-east-1
```

**Enclave config (baked into EIF — one CodeBuild project per environment):**
```
SECRET_ARN
KMS_KEY_ID
S3_BUCKET
VSOCK_PROXY_CID
OTEL_EXPORTER_OTLP_ENDPOINT
```

**Optional overrides:**
```
S3_PREFIX                    (default: schemas/)
SCHEMA_HEADER_NAME           (default: X-Schema-Name)
DEK_ROTATION_INTERVAL_SECS   (default: 3600)
SCHEMA_REFRESH_INTERVAL_SECS (default: 300)
VSOCK_PROXY_PORT             (default: 8000)
TLS_PORT                     (default: 443)
LOG_LEVEL                    (default: info)
```

### Build phases
1. **install**: Install `aws-nitro-enclaves-cli`, `jq`, Rust (rustup), `gcc/cmake`.
2. **pre_build**: ECR login; compute `IMAGE_TAG` from git SHA; run `cargo test`, `clippy`, `fmt`.
3. **build**:
   - Build enclave OCI image with all `--build-arg` values → `nitro-enc-svc-enclave:local`
   - `nitro-cli build-enclave` → `enclave/nitro-enc-svc.eif` + `enclave/pcr-values.json`
   - Build and push vsock-proxy image to ECR
   - Build and push runner image (contains the EIF) to ECR
4. **post_build**: Write `enclave/build-summary.json` with image tags and PCR values.

### Build artifacts
- `enclave/pcr-values.json` — raw nitro-cli output with PCR measurements
- `enclave/build-summary.json` — structured summary with ECR URIs and PCR values

---

## PCR Values and KMS Key Policy

### What PCR values are
PCR (Platform Configuration Register) values are cryptographic measurements of the
enclave image produced by `nitro-cli build-enclave`:

- **PCR0** — hash of the enclave image (binary + rootfs). Changes on every code or
  config change. Use this for KMS binding.
- **PCR1** — hash of the Linux kernel and bootstrap. Changes only with major AL2023 updates.
- **PCR2** — hash of the application. Equivalent to PCR0 for single-executable enclaves.

### KMS key policy update (required after every build)

After each CodeBuild run, read `enclave/build-summary.json` and update the KMS key
policy condition:

```json
{
  "Sid": "AllowNitroEnclaveDecrypt",
  "Effect": "Allow",
  "Principal": {
    "AWS": "arn:aws:iam::ACCOUNT_ID:role/ec2-enclave-node-role"
  },
  "Action": "kms:Decrypt",
  "Resource": "*",
  "Condition": {
    "StringEqualsIgnoreCase": {
      "kms:RecipientAttestation:PCR0": "PASTE_PCR0_FROM_BUILD_SUMMARY_HERE"
    }
  }
}
```

**This must be done before rolling out the new runner image**, otherwise the enclave
will fail to decrypt the DEK at startup (KMS attestation will reject the request).

### Recommended deployment gate
Use a CodePipeline manual approval step between the "build EIF" stage and the
"deploy runner image" stage to force a human to verify and update the KMS key policy.

---

## DaemonSet Deployment

### Node requirements
EKS nodes must:
- Be a [Nitro Enclave-capable EC2 instance type](https://docs.aws.amazon.com/enclaves/latest/user/nitro-enclave.html#nitro-enclave-reqs)
  (e.g., `c5.xlarge`, `m5.xlarge`, etc.)
- Have Nitro Enclaves enabled at launch (`--enclave-options 'Enabled=true'`)
- Have `nitro-enclaves-allocator` configured with sufficient memory and CPU reserved
  (must be ≥ `ENCLAVE_MEMORY_MB` and `ENCLAVE_CPU_COUNT`)
- Be labelled: `aws.amazon.com/nitro-enclaves: "true"` (for the DaemonSet `nodeSelector`)

### ENCLAVE_CID alignment

The CID is a vsock address. Three values must be consistent:

| Setting | Location | Must equal |
|---|---|---|
| `VSOCK_PROXY_CID` | Baked into EIF via `Dockerfile.enclave --build-arg` | The CID the enclave uses to reach `aws-vsock-proxy` on the parent EC2 |
| `ENCLAVE_CID` | `daemonset-enclave.yaml` env var → `run-enclave.sh` → `nitro-cli run-enclave --enclave-cid` | The CID vsock-proxy sidecars connect to |
| `ENCLAVE_CID` | `pod-example.yaml` vsock-proxy container env var | Same as above |

Conventionally: parent EC2 CID = 3 (reserved), enclave CID = 16 (or any fixed value ≥ 4).

### Rolling out a new version
```bash
# After CodeBuild completes and KMS key policy is updated with new PCR0:
kubectl set image daemonset/nitro-enclave \
  enclave-runner=<ECR_REGISTRY>/nitro-enc-svc/runner:<NEW_IMAGE_TAG> \
  -n nitro-enc-svc

# Monitor rollout (maxUnavailable=1, so nodes roll one at a time)
kubectl rollout status daemonset/nitro-enclave -n nitro-enc-svc
```

---

## Observability

Enclave application logs are streamed to the runner container's stdout by `run-enclave.sh`
via `nitro-cli console`. Access them with:
```bash
kubectl logs -n nitro-enc-svc -l app=nitro-enclave -f
```

The enclave also exports OTEL traces, metrics, and structured logs over vsock to the
OTEL Collector running on the parent EC2 instance (configured via
`OTEL_EXPORTER_OTLP_ENDPOINT` baked into the EIF).
