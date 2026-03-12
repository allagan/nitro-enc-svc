# nitro-enc-svc

High-throughput, low-latency PII field encryption service running inside AWS Nitro Enclaves on EKS.
Targets **1000s TPS** with **single-digit millisecond latency**.

- Receives HTTPS requests (TLS terminates inside the enclave)
- Identifies PII fields from OpenAPI specs (`x-pii: true`)
- Encrypts fields with AES-256-GCM-SIV, returns `v1.<nonce>.<ciphertext>`
- `POST /decrypt` reverses the operation using the same DEK

Full design: [`CLAUDE.md`](CLAUDE.md) | Infrastructure: [`terraform/DESIGN.md`](terraform/DESIGN.md) | Deployment ops: [`docs/deployment.md`](docs/deployment.md)

---

## Quickstart — Restart from Scratch on a New EC2

Everything you need to go from zero to a live encrypted service. Do these steps in order.

---

### 1. Launch an EC2 Instance (development/ops machine)

This machine is the **operator workstation** — it runs Terraform, builds nothing (CodeBuild handles that), and accesses the EKS cluster.

**Recommended:**
- **AMI**: Amazon Linux 2023 (`al2023-ami-*-x86_64`)
- **Instance type**: `t3.medium` or larger
- **IAM role**: must have admin or broad permissions (see below)
- **VPC/subnet**: default VPC is fine; the instance needs internet access
- **Storage**: 20 GiB gp3

**IAM role permissions needed on the EC2:**
- `AdministratorAccess` — simplest for a dev ops machine, or scope to:
  - `AmazonEKSClusterPolicy` + `AmazonEKSWorkerNodePolicy`
  - `AmazonEC2FullAccess`
  - `IAMFullAccess`
  - `AmazonS3FullAccess`
  - `AWSCodePipeline_FullAccess`
  - `AWSCodeBuildAdminAccess`
  - `SecretsManagerReadWrite`
  - `AWSKeyManagementServicePowerUser`

> **Note:** Record the public IP of this new EC2 — you need it for `public_access_cidrs` in `terraform.tfvars`.

---

### 2. Install System Dependencies

```bash
# Rust toolchain
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \
  | sh -s -- -y --default-toolchain stable
source ~/.cargo/env
rustup component add rustfmt clippy

# C toolchain (required by aws-lc-sys in the AWS SDK)
sudo dnf install -y gcc gcc-c++ cmake make openssl-devel pkg-config git

# Terraform
sudo dnf install -y yum-utils
sudo yum-config-manager --add-repo https://rpm.releases.hashicorp.com/AmazonLinux/hashicorp.repo
sudo dnf install -y terraform

# kubectl
curl -Lo /usr/local/bin/kubectl \
  "https://dl.k8s.io/release/v1.31.0/bin/linux/amd64/kubectl"
chmod +x /usr/local/bin/kubectl

# AWS CLI (already installed on AL2023; verify)
aws --version
```

---

### 3. Clone the Repository

```bash
git clone https://github.com/allagan/nitro-enc-svc.git
cd nitro-enc-svc
```

---

### 4. Update `terraform/terraform.tfvars`

Open `terraform/terraform.tfvars`. Most values are already correct for the `dev` environment in `us-east-2`. **You must update one thing:**

```hcl
# Replace with the public IP of your new EC2 instance.
# This controls who can reach the EKS API server from outside the VPC.
# Use "0.0.0.0/0" to allow access from anywhere (fine for short-lived dev clusters).
public_access_cidrs = ["<YOUR_EC2_PUBLIC_IP>/32"]
```

Everything else in `terraform.tfvars` is already wired to the right account, region, CodeStar connection, S3 prefix, etc.

---

### 5. Check the CodeStar GitHub Connection

The GitHub → CodePipeline connection was created once and lives in the AWS account independently of the EKS cluster. It should still be **Available** after destroying the cluster.

```bash
aws codestar-connections list-connections --region us-east-2 \
  --query 'Connections[*].{Name:ConnectionName,Status:ConnectionStatus,Arn:ConnectionArn}' \
  --output table
```

If the connection shows `PENDING`, complete it in the AWS Console:
**Developer Tools → Connections → select the connection → Update pending connection → authorize with GitHub.**

The ARN in `terraform.tfvars` (`codestar_connection_arn`) should already match. If you need to create a new connection, update that variable with the new ARN.

---

### 6. Initialize Terraform and Deploy Infrastructure

```bash
cd terraform

terraform init

# Preview what will be created (~50+ resources)
terraform plan -out tfplan

# Deploy: VPC, EKS, IAM roles, KMS keys, ECR repos, S3 buckets, CodePipeline
# Takes ~15 minutes (EKS cluster creation dominates)
terraform apply tfplan
```

When complete, configure `kubectl`:

```bash
aws eks update-kubeconfig --name nitro-enc-svc-dev --region us-east-2
kubectl get nodes   # should show 1-2 nodes in Ready state
```

---

### 7. Apply Kubernetes Manifests

```bash
cd ..   # back to repo root

kubectl apply -f deploy/namespace.yaml
kubectl apply -f deploy/rbac.yaml
kubectl apply -f deploy/otel-collector.yaml
kubectl apply -f deploy/daemonset-enclave.yaml
kubectl apply -f deploy/vsock-proxy-service.yaml
kubectl apply -f deploy/e2e-latency-canary.yaml

# Wait for the NLB to get a hostname (takes 2-3 minutes)
kubectl get svc vsock-proxy-nlb -n nitro-enc-svc -w
```

> The enclave DaemonSet pods will stay in `CrashLoopBackOff` or `Pending` until the EIF is built and the DEK is provisioned — that's expected.

---

### 8. Provision the DEK (one-time, out-of-band)

The DEK (Data Encryption Key) is provisioned out-of-band so that the 32-byte key material **never appears in Terraform state**. This only needs to be done once per environment.

```bash
# Get the KMS key ID and Secret ARN from Terraform outputs
KMS_KEY_ID=$(cd terraform && terraform output -raw kms_dek_key_id)
SECRET_ARN=$(cd terraform && terraform output -raw dek_secret_arn)

# Generate a random 32-byte DEK and encrypt it with KMS
DEK_B64=$(openssl rand -hex 32 | xxd -r -p | base64)
CIPHERTEXT=$(aws kms encrypt \
  --key-id "$KMS_KEY_ID" \
  --region us-east-2 \
  --plaintext "$DEK_B64" \
  --query CiphertextBlob \
  --output text | base64 -d)

# Store the KMS-encrypted DEK in Secrets Manager as binary
printf '%s' "$CIPHERTEXT" > /tmp/dek_ciphertext.bin
aws secretsmanager put-secret-value \
  --secret-id "$SECRET_ARN" \
  --region us-east-2 \
  --secret-binary fileb:///tmp/dek_ciphertext.bin

# Clean up — key material must not persist on disk
unset DEK_B64 && rm /tmp/dek_ciphertext.bin
echo "DEK provisioned successfully"
```

---

### 9. Upload OpenAPI Schemas to S3

The enclave loads PII field definitions from S3 at startup.

```bash
S3_BUCKET=$(cd terraform && terraform output -raw schemas_bucket_name)

aws s3 cp schemas/payments-v1.yaml \
  "s3://${S3_BUCKET}/schemas/payments-v1.yaml"

# Upload any additional schemas the same way:
# aws s3 cp schemas/your-schema.yaml "s3://${S3_BUCKET}/schemas/your-schema.yaml"

echo "Schemas uploaded to s3://${S3_BUCKET}/schemas/"
```

---

### 10. Trigger CodePipeline (Build Stage)

Push to `main` to auto-trigger the pipeline, or start it manually:

```bash
aws codepipeline start-pipeline-execution \
  --name nitro-enc-svc-dev \
  --region us-east-2
```

Monitor the Build stage in the AWS Console or CLI:

```bash
aws codepipeline get-pipeline-state \
  --name nitro-enc-svc-dev --region us-east-2 \
  --query 'stageStates[*].{Stage:stageName,Status:latestExecution.status}' \
  --output table
```

The Build stage takes ~10-15 minutes (installs Rust, runs tests, builds Docker images, produces EIF). It will:
1. Run `cargo fmt --check`, `cargo clippy`, `cargo test`
2. Build the enclave Docker image → EIF
3. Extract PCR0/PCR1/PCR2 from the EIF
4. Push `runner` and `vsock-proxy` images to ECR
5. Write `build-summary.json` to S3

---

### 11. Retrieve PCR0 and Update KMS Key Policy

After the Build stage succeeds, the pipeline pauses at the **Approve (ReviewPCR0)** gate.

```bash
# Get the PCR0 from the build artifacts
ARTIFACTS_BUCKET=$(cd terraform && terraform output -raw pipeline_artifacts_bucket_name)

aws s3 cp \
  "s3://${ARTIFACTS_BUCKET}/builds/build-summary.json" - | python3 -m json.tool
```

The output will look like:
```json
{
  "imageTag": "a217f24",
  "ecrRunner": "394582308905.dkr.ecr.us-east-2.amazonaws.com/nitro-enc-svc/dev/runner:a217f24",
  "ecrProxy":  "394582308905.dkr.ecr.us-east-2.amazonaws.com/nitro-enc-svc/dev/vsock-proxy:a217f24",
  "pcr0": "046ab9a3e29f64c50f9ade36dd7b978822155d0a21d71222f4890af280beae75...",
  "pcr1": "...",
  "pcr2": "..."
}
```

Apply the PCR0 to the KMS key policy **before** approving the pipeline gate:

```bash
PCR0=$(aws s3 cp "s3://${ARTIFACTS_BUCKET}/builds/build-summary.json" - \
  | python3 -c "import sys,json; print(json.load(sys.stdin)['pcr0'])")

cd terraform
terraform apply -var="kms_enclave_pcr0=${PCR0}" -auto-approve
cd ..
```

---

### 12. Approve the Pipeline Gate

```bash
# Get the approval token
TOKEN=$(aws codepipeline get-pipeline-state \
  --name nitro-enc-svc-dev --region us-east-2 \
  --query 'stageStates[?stageName==`Approve`].actionStates[0].latestExecution.token' \
  --output text)

# Approve
aws codepipeline put-approval-result \
  --pipeline-name nitro-enc-svc-dev \
  --stage-name Approve \
  --action-name ReviewPCR0 \
  --result "summary=PCR0 verified,status=Approved" \
  --token "$TOKEN" \
  --region us-east-2
```

The **DeployAndTest** stage now runs automatically. It:
1. Sets the DaemonSet image tags to the new build
2. Waits for rollout
3. Runs T-01 (health), T-02 (encrypt), T-03 (decrypt), T-04 (ab load test)

---

### 13. Verify the Deployment

```bash
# Get NLB hostname
NLB=$(kubectl get svc vsock-proxy-nlb -n nitro-enc-svc \
  -o jsonpath='{.status.loadBalancer.ingress[0].hostname}')
echo "NLB: $NLB"

# Health check
curl -sk "https://$NLB:8443/health" | python3 -m json.tool
# Expected: {"status":"ok","dek_ready":true,"schemas_loaded":1}

# Encrypt a PII field
curl -sk -X POST "https://$NLB:8443/encrypt" \
  -H "Content-Type: application/json" \
  -H "X-Schema-Name: payments-v1" \
  -d '{"payload":{"card_number":"4111111111111111","card_holder_name":"Jane Smith"}}' \
  | python3 -m json.tool
# Expected: card_number starts with "v1."

# Decrypt it back
ENCRYPTED=$(curl -sk -X POST "https://$NLB:8443/encrypt" \
  -H "Content-Type: application/json" -H "X-Schema-Name: payments-v1" \
  -d '{"payload":{"card_number":"4111111111111111"}}' \
  | python3 -c "import sys,json; print(json.load(sys.stdin)['payload']['card_number'])")

curl -sk -X POST "https://$NLB:8443/decrypt" \
  -H "Content-Type: application/json" \
  -H "X-Schema-Name: payments-v1" \
  -d "{\"payload\":{\"card_number\":\"$ENCRYPTED\"}}" \
  | python3 -m json.tool
# Expected: card_number = "4111111111111111"
```

> **NLB hairpin limitation**: test from any host *other than* the nitro node itself. From this
> EC2 (default VPC) you cannot reach the internal NLB. Either use SSM to run the curl commands
> on the general EKS node (`i-xxxxx`), or use a bastion in the EKS VPC.
>
> ```bash
> # Send test command via SSM to the general EKS node
> GENERAL_NODE=$(aws ec2 describe-instances --region us-east-2 \
>   --filters "Name=tag:Name,Values=nitro-enc-svc-dev-general" \
>             "Name=instance-state-name,Values=running" \
>   --query 'Reservations[0].Instances[0].InstanceId' --output text)
>
> aws ssm send-command \
>   --instance-ids "$GENERAL_NODE" \
>   --document-name "AWS-RunShellScript" \
>   --region us-east-2 \
>   --parameters "commands=[\"curl -sk https://$NLB:8443/health\"]" \
>   --query 'Command.CommandId' --output text
> ```

---

### 14. Tear Down (Save Costs)

When you're done, destroy everything:

```bash
# Empty versioned S3 buckets first (Terraform can't delete non-empty versioned buckets)
for BUCKET in \
  "$(cd terraform && terraform output -raw schemas_bucket_name)" \
  "$(cd terraform && terraform output -raw pipeline_artifacts_bucket_name)"; do
  echo "Emptying $BUCKET..."
  aws s3api list-object-versions --bucket "$BUCKET" --output json 2>/dev/null \
    | python3 -c "
import sys, json, subprocess
data = json.load(sys.stdin)
for k in ['Versions','DeleteMarkers']:
    for obj in data.get(k, []):
        subprocess.run(['aws','s3api','delete-object','--bucket','$BUCKET',
                        '--key',obj['Key'],'--version-id',obj['VersionId']], check=False)
"
done

# Force-delete ECR repos (contains images Terraform won't delete otherwise)
aws ecr delete-repository --repository-name nitro-enc-svc/dev/runner    --force --region us-east-2
aws ecr delete-repository --repository-name nitro-enc-svc/dev/vsock-proxy --force --region us-east-2

# Remove Karpenter Helm state (cluster is gone, provider can't uninstall it)
cd terraform && terraform state rm helm_release.karpenter

# Destroy everything else
terraform destroy -auto-approve -var-file="terraform.tfvars"
```

Or use the convenience script (handles steps 1–4 automatically):

```bash
./scripts/destroy.sh
```

> If `destroy.sh` fails mid-way with the S3/ECR errors above, run the manual steps and then re-run `terraform destroy`.

---

## Day-to-Day Development

### Build and Test Locally

```bash
source ~/.cargo/env

# Compile everything
cargo build --workspace

# Run all tests (71 tests across 3 crates)
cargo test --workspace

# Lint (zero warnings enforced)
cargo clippy --workspace --all-targets -- -D warnings

# Format
cargo fmt --all
```

### Deploy a Code Change

```bash
git add -p        # stage your changes
git commit -m "your message"
git push origin main
# Pipeline auto-triggers: Build → Approve → DeployAndTest
```

After the Build stage completes, run steps 11–12 (PCR0 retrieval and pipeline approval).

> Every change to `crates/enclave/` changes the EIF and thus the PCR0. Always retrieve the new
> PCR0 and run `terraform apply -var="kms_enclave_pcr0=<NEW_PCR0>"` before approving the gate,
> otherwise the enclave cannot decrypt the DEK and will fail to start.

### Using Claude Code (AI-assisted Development)

This repo was built with [Claude Code](https://claude.com/claude-code). To continue AI-assisted development:

```bash
# Install Claude Code (requires Node.js 18+)
npm install -g @anthropic-ai/claude-code

# Run from the repo root
cd nitro-enc-svc
claude

# Claude Code has persistent memory for this project in:
# ~/.claude/projects/-home-<user>-nitro-enc-svc/memory/MEMORY.md
# It knows the architecture, current state, and prior decisions.
```

The `CLAUDE.md` file at the repo root contains project-specific instructions that Claude Code reads automatically on every session.

---

## Repository Structure

```
nitro-enc-svc/
├── CLAUDE.md                   # Project guide (architecture, conventions, commands)
├── README.md                   # This file — restart guide
├── Cargo.toml                  # Workspace root
├── buildspec.yml               # CodeBuild: build + test + EIF production
├── buildspec-test.yml          # CodeBuild: post-deploy smoke tests (T-01 … T-04)
├── Dockerfile.enclave          # Enclave OCI image (input to nitro-cli)
├── Dockerfile.runner           # Runner container (manages enclave lifecycle)
├── Dockerfile.vsock-proxy      # vsock-proxy sidecar container
├── crates/
│   ├── enclave/src/
│   │   ├── main.rs             # Startup: vsock bridges → telemetry → DEK → schemas → TLS server
│   │   ├── crypto/cipher.rs    # AES-256-GCM-SIV encrypt/decrypt
│   │   ├── dek/mod.rs          # DEK fetch (Secrets Manager → KMS) + rotation task
│   │   ├── schema/mod.rs       # OpenAPI schema load from S3 + refresh task
│   │   ├── server/handlers.rs  # POST /encrypt, POST /decrypt, GET /health
│   │   └── telemetry/          # OTEL metrics, traces, structured log forwarding
│   ├── vsock-proxy/            # TCP ↔ vsock tunnel sidecar
│   └── common/                 # Shared protocol types (EncryptRequest/Response, etc.)
├── deploy/
│   ├── namespace.yaml          # Namespace: nitro-enc-svc
│   ├── rbac.yaml               # ServiceAccount: nitro-enclave-runner
│   ├── daemonset-enclave.yaml  # Nitro Enclave runner DaemonSet (one per nitro node)
│   ├── vsock-proxy-service.yaml # vsock-proxy DaemonSet + internal NLB Service
│   ├── otel-collector.yaml     # ADOT Collector DaemonSet → CloudWatch Logs/Metrics/X-Ray
│   ├── e2e-latency-canary.yaml # CronJob: e2e latency → CloudWatch custom metric
│   ├── karpenter-ec2nodeclass.yaml
│   └── karpenter-nodepool.yaml
├── schemas/
│   └── payments-v1.yaml        # Example OpenAPI spec with x-pii: true fields
├── terraform/
│   ├── terraform.tfvars        # All environment config (account, region, ARNs, etc.)
│   ├── acm.tf                  # ACM certificate for Nitro Enclaves (optional)
│   ├── eks.tf                  # EKS cluster, node groups, Karpenter, addons
│   ├── iam.tf                  # All IAM roles and policies
│   ├── kms.tf                  # KMS keys (DEK, EKS secrets, EBS) with PCR0 gate
│   ├── vpc.tf                  # VPC, subnets, NAT gateways, flow logs
│   ├── ecr.tf                  # ECR repos (runner, vsock-proxy)
│   ├── storage.tf              # S3 buckets (schemas, pipeline artifacts) + SM secret
│   ├── pipeline.tf             # CodeBuild + CodePipeline CI/CD
│   └── templates/
│       └── node_userdata.sh.tpl  # Nitro node bootstrap (allocator + vsock systemd units)
├── scripts/
│   ├── deploy.sh               # One-command full deploy
│   └── destroy.sh              # One-command full teardown
└── docs/
    ├── deployment.md           # Ops runbook (Karpenter, smoke tests, troubleshooting)
    ├── benchmark-*.md          # TPS/latency benchmarks
    └── test-report-*.md        # Pipeline test reports
```

---

## API Reference

### POST /encrypt

Encrypts PII fields identified by the OpenAPI schema in `X-Schema-Name`.

```bash
curl -sk -X POST "https://<NLB>:8443/encrypt" \
  -H "Content-Type: application/json" \
  -H "X-Schema-Name: payments-v1" \
  -d '{"payload":{"card_number":"4111111111111111","card_holder_name":"Jane Smith"}}'
```

Response: `{"payload":{"card_number":"v1.<nonce>.<ciphertext>","card_holder_name":"v1.<nonce>.<ciphertext>"}}`

### POST /decrypt

Decrypts `v1.<nonce>.<ciphertext>` fields back to plaintext. Non-encrypted fields at PII paths are left unchanged.

```bash
curl -sk -X POST "https://<NLB>:8443/decrypt" \
  -H "Content-Type: application/json" \
  -H "X-Schema-Name: payments-v1" \
  -d '{"payload":{"card_number":"v1.<nonce>.<ciphertext>"}}'
```

Response: `{"payload":{"card_number":"4111111111111111"}}`

### GET /health

```bash
curl -sk "https://<NLB>:8443/health"
# 200 OK: {"status":"ok","dek_ready":true,"schemas_loaded":1}
# 503:    {"status":"degraded","dek_ready":false,"schemas_loaded":0}
```

---

## Key AWS Resources (dev environment)

| Resource | Value |
|---|---|
| Region | `us-east-2` |
| Account | `394582308905` |
| EKS Cluster | `nitro-enc-svc-dev` |
| CodePipeline | `nitro-enc-svc-dev` |
| ECR (runner) | `394582308905.dkr.ecr.us-east-2.amazonaws.com/nitro-enc-svc/dev/runner` |
| ECR (proxy) | `394582308905.dkr.ecr.us-east-2.amazonaws.com/nitro-enc-svc/dev/vsock-proxy` |
| S3 (schemas) | `dev-nitro-enc-svc-schemas-394582308905` |
| GitHub repo | `allagan/nitro-enc-svc` |
| KMS alias (DEK) | `alias/nitro-enc-svc/dev/dek` |
| SM secret | `nitro-enc-svc/dev/dek` |
| CloudWatch logs | `/nitro-enc-svc/dev/enclave` |
| CloudWatch metrics | `NitroEncSvc/Dev` |
| CloudWatch dashboard | `NitroEncSvc-Dev` |

---

## Architecture Summary

```
Client → NLB (internal, port 8443)
           │
           ▼ externalTrafficPolicy: Local
         vsock-proxy DaemonSet Pod   (TCP ↔ vsock tunnel, TLS pass-through)
           │  vsock CID=16, port=443
           ▼
         Nitro Enclave   (TLS terminates here)
           • rustls TLS server (vsock listener)
           • Parses X-Schema-Name header → PII field paths
           • Encrypts/decrypts fields with AES-256-GCM-SIV DEK
           │
           │  vsock → parent EC2 aws-vsock-proxy → AWS KMS / Secrets Manager / S3
           │  vsock → parent EC2 OTEL Collector  → CloudWatch Logs / Metrics / X-Ray

EKS node: c5.xlarge (Nitro Enclaves enabled, enclave_options=true)
Enclave: 2 vCPU, 1024 MiB, CID=16
```
