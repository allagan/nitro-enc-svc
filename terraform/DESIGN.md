# Terraform IaC Design — nitro-enc-svc

## Overview

This Terraform module provisions all AWS infrastructure required to build, deploy, and run
`nitro-enc-svc` — a high-throughput PII field encryption service running inside AWS Nitro
Enclaves on EKS. It creates everything from scratch: VPC, EKS cluster, CI/CD pipeline, KMS
keys, IAM roles, ECR repositories, and S3 storage.

Target: **1000s TPS**, **single-digit millisecond latency**, with cryptographic attestation
binding the KMS decryption key to a specific, measured enclave image.

---

## Infrastructure Map

```
                          ┌─────────────────────────────────────────────┐
                          │                    VPC                       │
                          │  10.0.0.0/16  (3 AZs)                       │
                          │                                              │
                          │  Public subnets  10.0.0.0/20  (×3)          │
                          │  ├── NAT Gateway (one per AZ)                │
                          │  └── Internet Gateway                        │
                          │                                              │
                          │  Private subnets  10.0.48.0/20 (×3)         │
                          │  ├── EKS control plane ENIs                  │
                          │  ├── General node group  (m5.large ×2)       │
                          │  └── Nitro node group    (c5.xlarge ×2)      │
                          └─────────────────────────────────────────────┘
                                           │
                          ┌────────────────▼────────────────────────────┐
                          │              EKS Cluster                     │
                          │                                              │
                          │  Addons (Pod Identity):                      │
                          │  ├── eks-pod-identity-agent                  │
                          │  ├── vpc-cni          ← Pod Identity role    │
                          │  ├── coredns                                 │
                          │  ├── kube-proxy                              │
                          │  └── aws-ebs-csi-driver ← Pod Identity role  │
                          │                                              │
                          │  Workloads (deploy/ manifests):              │
                          │  └── nitro-enclave DaemonSet                 │
                          │      (scheduled only on Nitro nodes)         │
                          └─────────────────────────────────────────────┘
                                           │
          ┌────────────────────────────────┼───────────────────────────┐
          │                                │                           │
          ▼                                ▼                           ▼
  ┌──────────────┐              ┌────────────────────┐      ┌──────────────────┐
  │  KMS (×3)    │              │  CodePipeline       │      │  ECR (×2)        │
  │              │              │                     │      │                  │
  │  enclave-dek │              │  Source             │      │  runner          │
  │  eks-secrets │              │  (GitHub/CodeStar)  │      │  vsock-proxy     │
  │  ebs         │              │       ↓             │      │                  │
  └──────────────┘              │  Build (CodeBuild)  │      └──────────────────┘
          │                     │  • cargo build      │
          │                     │  • docker build     │
          ▼                     │  • nitro-cli build  │
  ┌──────────────┐              │  • extract PCR0     │
  │  Secrets Mgr │              │       ↓             │
  │              │              │  Approve            │
  │  dek secret  │              │  (manual gate)      │
  └──────────────┘              └────────────────────┘
          │
          ▼
  ┌──────────────┐
  │  S3 (×2)     │
  │              │
  │  schemas     │
  │  artifacts   │
  └──────────────┘
```

---

## File-by-File Reference

### `versions.tf`

Pins the AWS provider to `~> 5.0` and sets `required_version = ">= 1.6"`.

The S3 backend block is commented out. Before using this module in a team or CI environment,
uncomment it and supply a bucket, key, region, and DynamoDB lock table:

```hcl
backend "s3" {
  bucket         = "<tfstate-bucket>"
  key            = "nitro-enc-svc/<env>/terraform.tfstate"
  region         = "<region>"
  dynamodb_table = "<lock-table>"
  encrypt        = true
}
```

`default_tags` injects `Project`, `Environment`, and `ManagedBy = "terraform"` on every resource
automatically.

---

### `variables.tf`

All configuration is driven by variables. No hard-coded account IDs or region names appear
anywhere in the `.tf` files.

Key variables and their purpose:

| Variable | Purpose |
|---|---|
| `environment` | Validated to `dev \| staging \| prod`; used in resource names and tags |
| `account_id` | Validated 12-digit string; used in KMS key policies and S3 bucket names |
| `cluster_version` | EKS Kubernetes version; also drives the SSM AMI lookup |
| `availability_zones` | Validated to exactly 3; drives subnet and NAT gateway creation |
| `nitro_instance_types` | Must be Nitro Enclave-capable (c5, m5, r5, c5n, etc.) |
| `kms_enclave_pcr0` | SHA-384 PCR0 hash; empty by default — see attestation gate design below |
| `public_access_cidrs` | Restricts who can reach the EKS public API endpoint |

---

### `vpc.tf`

#### Subnet addressing

Given `vpc_cidr = "10.0.0.0/16"` and `availability_zones = ["us-east-1a","us-east-1b","us-east-1c"]`,
`cidrsubnet` produces:

| Subnet | CIDR | AZ |
|---|---|---|
| public-0 | 10.0.0.0/20 | us-east-1a |
| public-1 | 10.0.16.0/20 | us-east-1b |
| public-2 | 10.0.32.0/20 | us-east-1c |
| private-0 | 10.0.48.0/20 | us-east-1a |
| private-1 | 10.0.64.0/20 | us-east-1b |
| private-2 | 10.0.80.0/20 | us-east-1c |

Each subnet is tagged with `kubernetes.io/cluster/<name>` and the appropriate ELB role tag so
that the AWS Load Balancer Controller (if added later) can discover subnets automatically.

#### NAT Gateway strategy

Three NAT Gateways (one per AZ) provide high-availability egress. Each private subnet routes
outbound traffic through the NAT Gateway in the same AZ, avoiding cross-AZ data transfer costs.

For cost-sensitive dev environments you can reduce this to a single NAT Gateway by modifying
`aws_nat_gateway` and `aws_route_table.private` to use `count = 1`.

#### VPC Flow Logs

All traffic (ACCEPT + REJECT) is captured to a CloudWatch Logs group with 30-day retention.
This is required for security auditing and incident response in regulated environments.

---

### `kms.tf`

Three separate KMS keys are created with the principle of least-privilege key usage.

#### Key 1: `enclave_dek` — Data Encryption Key wrapping

Purpose: encrypts/decrypts the 32-byte AES-256-GCM-SIV DEK that the enclave uses for field
encryption.

**Key policy statements:**

1. `AllowRootAdmin` — unconditional admin access for the account root; prevents the key from
   becoming unmanageable if the enclave node role is accidentally deleted.

2. `AllowCodeBuild` — `kms:GenerateDataKey`, `kms:Encrypt`, `kms:DescribeKey` for the CodeBuild
   role. CodeBuild needs to wrap a freshly-generated DEK ciphertext during the initial setup
   workflow.

3. `AllowEnclaveDecryptWithAttestation` *(dynamic — only present when `kms_enclave_pcr0 != ""`)*:
   - Principal: `aws_iam_role.enclave_node`
   - Actions: `kms:Decrypt`, `kms:DescribeKey`
   - Condition: `kms:RecipientAttestation:PCR0 == var.kms_enclave_pcr0`

   This condition is enforced by the KMS Nitro Enclaves attestation protocol. KMS verifies
   the signed attestation document produced by the enclave hardware and only releases the
   plaintext DEK when PCR0 matches. No other process on the parent EC2 instance can decrypt
   the DEK, even if it has the enclave node role credentials.

#### Key 2: `eks_secrets` — Kubernetes Secrets envelope encryption

Encrypts the data encryption key that Kubernetes uses to encrypt Secret objects in etcd.
Without this, Kubernetes Secrets (including service account tokens and ConfigMaps containing
sensitive data) are stored in plaintext in etcd.

The EKS cluster role is granted `Encrypt`, `Decrypt`, `ReEncrypt*`, `GenerateDataKey*`, and
`DescribeKey` so the control plane can transparently encrypt and decrypt Secret resources.

#### Key 3: `ebs` — Node EBS volume encryption

Encrypts root EBS volumes on Nitro Enclave nodes. The EC2 Auto Scaling service-linked role
is granted the necessary KMS actions and `CreateGrant` (with `kms:GrantIsForAWSResource`
condition) so that Auto Scaling can pass the key to EC2 when launching instances.

---

### `iam.tf`

#### Role architecture

```
eks.amazonaws.com
    └── eks_cluster role
            └── AmazonEKSClusterPolicy

ec2.amazonaws.com
    ├── general_node role
    │       ├── AmazonEKSWorkerNodePolicy
    │       ├── AmazonEKS_CNI_Policy
    │       └── AmazonEC2ContainerRegistryReadOnly
    │
    └── enclave_node role
            ├── AmazonEKSWorkerNodePolicy
            ├── AmazonEC2ContainerRegistryReadOnly
            ├── (inline) enclave-kms        → kms:Decrypt + DescribeKey on enclave_dek
            ├── (inline) enclave-secretsmanager → secretsmanager:GetSecretValue on dek secret
            └── (inline) enclave-s3         → s3:GetObject + ListBucket on schemas bucket

pods.eks.amazonaws.com  (Pod Identity trust)
    ├── vpc_cni_pod_identity role
    │       └── AmazonEKS_CNI_Policy
    │
    └── ebs_csi_pod_identity role
            ├── AmazonEBSCSIDriverPolicy
            └── (inline) ebs-csi-kms        → kms:* on ebs key

codebuild.amazonaws.com
    └── codebuild role
            ├── (inline) codebuild-logs       → CloudWatch Logs
            ├── (inline) codebuild-ecr        → push to both ECR repos
            ├── (inline) codebuild-s3         → artifacts bucket R/W + schemas bucket R
            ├── (inline) codebuild-kms        → wrap/unwrap DEK + EBS key access
            └── (inline) codebuild-secretsmanager → GetSecretValue on dek secret

codepipeline.amazonaws.com
    └── codepipeline role
            └── (inline) S3 R/W + CodeBuild StartBuild + CodeStar UseConnection + KMS
```

#### Why the enclave uses the EC2 node role (not Pod Identity)

Pod Identity associates an IAM role with a Kubernetes service account. The enclave process
(`nitro-enc-svc`) runs **inside** the Nitro Enclave — it is not a Kubernetes pod and has no
access to the K8s API server or the Pod Identity agent webhook. The enclave communicates with
AWS services through the `aws-vsock-proxy` running on the parent EC2 instance, which uses the
**EC2 instance profile** (IMDS) to sign requests. Therefore the enclave's IAM permissions are
on the `enclave_node` EC2 role, not on a Pod Identity association.

---

### `eks.tf`

#### Cluster configuration

- **Private + public endpoint**: private access enables pod-to-API traffic to stay within the
  VPC; the public endpoint is restricted to `var.public_access_cidrs` (default `0.0.0.0/0`,
  recommended to restrict to a corporate CIDR in production).
- **All control plane log types** sent to CloudWatch (90-day retention): `api`, `audit`,
  `authenticator`, `controllerManager`, `scheduler`.
- **Secrets encryption**: `encryption_config` block references the `eks_secrets` KMS key.
  Kubernetes Secrets are envelope-encrypted before being written to etcd.
- **Access mode**: `API_AND_CONFIG_MAP` — supports both the newer EKS Access Entries API and
  the legacy `aws-auth` ConfigMap for backward compatibility.

#### General node group

Uses the EKS-managed node group (no custom launch template). Nodes are tainted
`CriticalAddonsOnly=true:NoSchedule` so only system-critical pods (CoreDNS, etc.) land here.
Regular application pods are scheduled on the nitro nodes.

#### Nitro Enclave launch template

The custom launch template is required because EKS-managed node groups do not expose
`enclave_options`. Key settings:

- `enclave_options { enabled = true }` — activates the Nitro Enclave hypervisor on the instance.
  This cannot be changed after launch; the instance must be replaced.
- `metadata_options { http_tokens = "required" }` — enforces IMDSv2. Prevents SSRF attacks
  from reaching the IMDS endpoint with a simple HTTP GET.
- `http_put_response_hop_limit = 1` — prevents containers from reaching IMDS (hop limit of 1
  means the request cannot cross the network bridge into a container).
- Encrypted root volume (gp3, 50 GiB) using the `ebs` KMS key.
- User data runs `nitro-enclaves-allocator` configuration before the EKS bootstrap script
  (see `templates/node_userdata.sh.tpl`).

#### EKS addons and Pod Identity

The **Pod Identity Agent** addon (`eks-pod-identity-agent`) must be installed and running
before creating `aws_eks_pod_identity_association` resources. Terraform `depends_on` chains
enforce this ordering.

Pod Identity associations are explicit mappings:

| Addon | Namespace | Service Account | IAM Role |
|---|---|---|---|
| vpc-cni | kube-system | aws-node | vpc_cni_pod_identity |
| aws-ebs-csi-driver | kube-system | ebs-csi-controller-sa | ebs_csi_pod_identity |

Pod Identity trust policies use `pods.eks.amazonaws.com` as the service principal with both
`sts:AssumeRole` and `sts:TagSession` actions. The `TagSession` action is required for the
Pod Identity credential exchange to complete.

---

### `ecr.tf`

Both repositories (`runner` and `vsock-proxy`) are configured with:

- **`IMMUTABLE` tag mutability** — prevents overwriting an existing image tag. Every push must
  use a new tag (commit SHA or semantic version). This ensures the image version running in
  production can always be identified and reproduced.
- **Scan on push** — Amazon Inspector scans each layer for known CVEs when the image is pushed.
  Scan results appear in the ECR console and can trigger EventBridge rules for alerting.
- **KMS encryption** — image layers are encrypted at rest using the `ebs` KMS key.
- **Lifecycle policy** — keeps the last 10 tagged images and expires untagged images after 1 day
  to prevent unbounded storage growth.

---

### `storage.tf`

#### S3 hardening (applied to both buckets)

| Control | Setting |
|---|---|
| Versioning | Enabled — allows recovery from accidental deletes or overwrites |
| Server-side encryption | SSE-KMS with `enclave_dek` key + bucket key enabled (reduces KMS API calls) |
| Public access block | All 4 flags set to `true` — prevents any public access regardless of ACLs or bucket policy |
| Bucket policy | Explicit `Deny` on `aws:SecureTransport = false` — all requests must use HTTPS |

#### Secrets Manager DEK secret

The secret resource is created with:
- KMS key: `enclave_dek` (double-wrapped: SM encrypts with KMS, KMS requires attestation for Decrypt)
- 30-day recovery window: prevents accidental permanent deletion
- **No `aws_secretsmanager_secret_version` resource** — the DEK value is provisioned out-of-band

Out-of-band DEK provisioning keeps key material out of Terraform state files (which may be
stored in S3, shared with CI, or kept in version control history). The storage.tf file contains
step-by-step CLI instructions for generating and wrapping the DEK.

---

### `pipeline.tf`

#### CodeBuild project

The build project runs in a `LINUX_CONTAINER` with `privileged_mode = true` (required to run
a Docker daemon). The buildspec (`buildspec.yml` in the repository root) is expected to:

1. Build the Rust workspace (`cargo build --release`)
2. Build Docker images for `runner` and `vsock-proxy`
3. Run `nitro-cli build-enclave` to produce an EIF
4. Extract PCR0/PCR1/PCR2 measurements from the EIF
5. Push Docker images to ECR
6. Write `pcr-values.json` and `build-summary.json` as secondary S3 artifacts

All service configuration (DEK ARN, KMS key ID, S3 bucket, vsock CIDs, OTEL endpoint, etc.)
is injected as environment variables into the build environment.

Secondary artifacts are uploaded to `pipeline_artifacts/builds/` in S3, separate from the
primary CODEPIPELINE artifact store, so they remain accessible after the pipeline run completes.

#### CodePipeline stages

```
GitHub (CodeStar connection)
    │  source_output (ZIP)
    ▼
CodeBuild
    │  build_output
    │  pcr-values.json    → S3 artifacts bucket
    │  build-summary.json → S3 artifacts bucket
    ▼
Manual Approval
    │  Reviewer reads build-summary.json
    │  Verifies PCR0 matches expected value
    │  Updates kms_enclave_pcr0 in Terraform + re-applies
    │  Then approves the gate
    ▼
  (future: EKS deploy stage)
```

The manual approval gate is intentionally positioned after the build so that the operator can
inspect the PCR0 hash produced by the build before allowing the pipeline to proceed. Without a
matching PCR0 in the KMS key policy, the enclave cannot decrypt the DEK and the service will
not start — but this gate provides a human checkpoint to catch unexpected image changes.

---

### `templates/node_userdata.sh.tpl`

Template variables: `${cluster_name}`, `${enclave_memory_mb}`, `${enclave_cpu_count}`.

The script runs with `set -euo pipefail` so any failure aborts the bootstrap and the instance
is marked unhealthy by Auto Scaling.

**Ordering matters**: `nitro-enclaves-allocator` must be configured and restarted **before**
`/etc/eks/bootstrap.sh` runs. The bootstrap script starts the `kubelet`, which immediately
registers the node with the cluster. If the allocator is not already configured, the DaemonSet
pod can start before memory/CPU are reserved for the enclave, causing `nitro-cli run-enclave`
to fail.

---

## Key Design Decisions

### 1. PCR0 attestation gate — closed by default

The KMS `Decrypt` permission for the enclave node role is controlled by a `dynamic` block in
`kms.tf`. When `var.kms_enclave_pcr0 = ""` (the default), the block produces no statements —
the enclave node role has **no Decrypt permission at all** on the DEK key.

This is intentional: the PCR0 hash does not exist until the first successful EIF build. The
deployment workflow is:

```
terraform apply                  # PCR0 = "" → no Decrypt permission
    │
    ▼
Trigger CodePipeline             # builds EIF, extracts PCR0
    │
    ▼
Read build-summary.json          # get the SHA-384 PCR0 string
    │
    ▼
terraform apply \                # adds Decrypt + PCR0 condition to KMS policy
  -var="kms_enclave_pcr0=<hash>"
    │
    ▼
Approve CodePipeline gate        # enclave can now decrypt DEK
```

After a code change that alters the enclave binary, the PCR0 changes. The old policy
becomes invalid, the enclave cannot decrypt the DEK, and service restarts will fail until the
operator reviews the new PCR0 and re-applies Terraform. This creates a mandatory human review
step for every enclave image change.

### 2. Three KMS keys

Separate keys for DEK encryption, EKS Secrets encryption, and EBS volume encryption follow
the principle of least-privilege key usage:

- If the EBS key is compromised, the attacker cannot decrypt the DEK.
- If the DEK key is rotated or deleted, EKS Secret encryption and EBS volumes are unaffected.
- CloudTrail key usage logs cleanly attribute operations to their purpose.

### 3. Pod Identity vs. IRSA

EKS Pod Identity (not IRSA) is used for vpc-cni and ebs-csi-driver. Pod Identity:
- Requires no OIDC provider resource in Terraform (simplifying the module).
- Uses a per-association credential injection mechanism rather than OIDC token exchange.
- Is the recommended approach for new clusters on EKS 1.24+.

The `hashicorp/tls` provider (needed for OIDC thumbprints with IRSA) is explicitly **not**
included in `versions.tf`.

### 4. Separate node groups

The `general` node group is tainted `CriticalAddonsOnly:NoSchedule` to reserve it for system
pods (CoreDNS, vpc-cni DaemonSet, kube-proxy). The `nitro` node group carries the
`aws.amazon.com/nitro-enclaves=true` label, which the DaemonSet `nodeSelector` in `deploy/`
targets. This prevents the enclave runner from accidentally landing on a node without enclave
support enabled.

### 5. No Terraform Kubernetes resources

Kubernetes objects (Namespace, RBAC, DaemonSet) remain as YAML in `deploy/`. Adding
`kubernetes` or `helm` providers to the same root module as the EKS cluster creates a
chicken-and-egg dependency: the provider needs a working cluster to plan, but the cluster
is created by the same `terraform apply`. Keeping them separate avoids this and allows
independent lifecycle management of the cluster and the workloads.

### 6. IMDSv2 enforced on all Nitro nodes

`http_tokens = "required"` and `http_put_response_hop_limit = 1` in the launch template:
- Require a session-oriented IMDS token for all metadata requests (prevents SSRF-based
  credential theft).
- Prevent containers from reaching IMDS across the docker bridge (hop count > 1 is dropped).

### 7. ECR tag immutability

`image_tag_mutability = "IMMUTABLE"` ensures that a running pod's image reference is
permanently tied to a specific layer digest. Without this, a `latest` or `v1.2.3` tag could
be silently overwritten with a different image while pods using that tag remain running,
creating a gap between what you see in the tag and what is actually executing.

---

## Operational Runbook

### First-time setup

```bash
# 1. Copy and fill in the example vars file
cp terraform.tfvars.example terraform.tfvars
# Edit terraform.tfvars — at minimum set:
#   aws_region, environment, account_id, cluster_name, availability_zones
#   codestar_connection_arn, source_repo_id, vsock_proxy_cid, otel_otlp_endpoint

# 2. Initialise Terraform
terraform init

# 3. First apply (kms_enclave_pcr0 is empty — attestation gate closed)
terraform plan -out tfplan
terraform apply tfplan

# 4. Configure kubectl
aws eks update-kubeconfig --name <cluster_name> --region <region>
kubectl get nodes

# 5. Apply Kubernetes manifests
kubectl apply -f ../deploy/namespace.yaml
kubectl apply -f ../deploy/rbac.yaml
kubectl apply -f ../deploy/daemonset-enclave.yaml
# Pods will CrashLoopBackOff until the EIF is built — expected

# 6. Approve the GitHub CodeStar connection in the AWS Console (one-time)
#    Developer Tools → Connections → select the pending connection → Update pending connection

# 7. Provision the DEK out-of-band (see storage.tf comments for full CLI commands)
#    Summary:
DEK_HEX=$(openssl rand -hex 32)
aws kms encrypt \
  --key-id $(terraform output -raw kms_dek_key_id) \
  --plaintext "$(echo -n $DEK_HEX | xxd -r -p | base64)" \
  --query CiphertextBlob --output text | base64 -d > dek_ciphertext.bin
aws secretsmanager put-secret-value \
  --secret-id $(terraform output -raw dek_secret_arn) \
  --secret-binary fileb://dek_ciphertext.bin
unset DEK_HEX && rm dek_ciphertext.bin

# 8. Trigger CodePipeline (push a commit, or start execution manually)
aws codepipeline start-pipeline-execution \
  --name $(terraform output -raw codepipeline_name)

# 9. Wait for the Build stage to complete, then retrieve PCR0
aws s3 cp \
  s3://$(terraform output -raw pipeline_artifacts_bucket_name)/builds/build-summary.json \
  - | jq '.pcr0'

# 10. Re-apply with PCR0 to open the attestation gate
terraform apply -var="kms_enclave_pcr0=<PCR0_FROM_BUILD_SUMMARY>"

# 11. Approve the CodePipeline manual gate
aws codepipeline put-approval-result \
  --pipeline-name $(terraform output -raw codepipeline_name) \
  --stage-name Approve \
  --action-name ReviewPCR0 \
  --result "summary=PCR0 verified,status=Approved" \
  --token <approval-token>

# 12. Verify
kubectl rollout status daemonset/nitro-enclave -n nitro-enc-svc
```

### Rotating the DEK

The enclave automatically re-fetches the DEK from Secrets Manager every `dek_rotation_interval_secs`
seconds. To rotate:

1. Generate a new DEK and wrap it with KMS (same process as step 7 above).
2. `aws secretsmanager put-secret-value` — the new version becomes `AWSCURRENT`.
3. Wait for the rotation interval; the enclave will pick up the new DEK on the next refresh.
4. Verify with CloudTrail: look for `kms:Decrypt` calls from the enclave node role after the
   rotation interval passes.

### Changing the enclave image

Any change to the enclave binary (`crates/enclave`) changes the PCR0 measurement:

1. Merge the change to the source branch.
2. CodePipeline builds a new EIF and writes the new PCR0 to `build-summary.json`.
3. The old PCR0 condition in the KMS key policy no longer matches → the running enclave
   continues to work (it holds the DEK in memory), but **new pod starts will fail** until
   you update the policy.
4. Verify the new PCR0, then:
   ```bash
   terraform apply -var="kms_enclave_pcr0=<NEW_PCR0>"
   ```
5. Approve the pipeline gate.
6. Roll the DaemonSet: `kubectl rollout restart daemonset/nitro-enclave -n nitro-enc-svc`

---

## Security Controls Summary

| Threat | Control |
|---|---|
| Tampered enclave binary decrypts PII | PCR0 attestation condition on KMS Decrypt |
| Compromised EC2 host decrypts DEK | PCR0 condition — host cannot satisfy attestation |
| S3 schema files served over HTTP | Bucket policy denies `aws:SecureTransport = false` |
| Public S3 access | All public access block flags enabled |
| Container escapes reaching IMDS | IMDSv2 required + hop limit = 1 |
| etcd Secrets readable without KMS | EKS Secrets encrypted with `eks_secrets` KMS key |
| EBS snapshots expose node data | EBS volumes encrypted with `ebs` KMS key |
| Stale image tags overwritten | ECR tag immutability |
| Image layer vulnerabilities | ECR scan on push |
| Network traffic not audited | VPC Flow Logs (all traffic, 30-day retention) |
| Control plane activity not audited | EKS control plane logs to CloudWatch (90 days) |
| Terraform state contains DEK | DEK provisioned out-of-band, no state secret version |
| Key misuse across purposes | Three separate KMS keys (DEK / EKS secrets / EBS) |
