#!/bin/bash
# destroy.sh — Full one-command teardown for nitro-enc-svc.
#
# Usage:
#   ./scripts/destroy.sh [path/to/tfvars]
#
# Environment variables:
#   AWS_DEFAULT_REGION   — defaults to us-east-2
#   CLUSTER_NAME         — defaults to nitro-enc-svc-dev
#
# Steps:
#   1. Delete Karpenter NodePool (triggers graceful node termination)
#   2. Wait for nitro nodes to terminate
#   3. Delete Karpenter EC2NodeClass + placeholder
#   4. Delete remaining application manifests
#   5. terraform destroy (removes VPC, EKS, IAM, KMS, S3, ECR, CodePipeline)

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
REGION="${AWS_DEFAULT_REGION:-us-east-2}"
CLUSTER="${CLUSTER_NAME:-nitro-enc-svc-dev}"
TFVARS="${1:-$ROOT/terraform/terraform.tfvars}"

echo "=== nitro-enc-svc destroy ==="
echo "  Cluster:  $CLUSTER"
echo "  Region:   $REGION"
echo "  Tfvars:   $TFVARS"
echo ""
echo "WARNING: This will destroy ALL infrastructure including EKS, KMS keys,"
echo "         S3 buckets, ECR repos, and CodePipeline."
echo ""

# ── Kubeconfig (best-effort; cluster may already be gone) ─────────────────────

aws eks update-kubeconfig --name "$CLUSTER" --region "$REGION" 2>/dev/null || true

# ── 1. Delete Karpenter NodePool (graceful node drain + termination) ──────────

echo "--- [1/5] delete Karpenter NodePool (triggers node termination) ---"
kubectl delete -f "$ROOT/deploy/karpenter-nodepool.yaml" \
  --ignore-not-found --timeout=120s 2>/dev/null || true

# ── 2. Wait for nitro nodes to terminate ─────────────────────────────────────

echo "--- [2/5] waiting for nitro nodes to terminate (up to 5 min) ---"
kubectl wait node \
  -l "aws.amazon.com/nitro-enclaves=true" \
  --for=delete \
  --timeout=300s 2>/dev/null || true

echo "  Nitro nodes terminated (or none found)"

# ── 3. Delete Karpenter EC2NodeClass + placeholder ────────────────────────────

echo "--- [3/5] delete EC2NodeClass + placeholder ---"
kubectl delete -f "$ROOT/deploy/karpenter-ec2nodeclass.yaml" \
  --ignore-not-found --timeout=60s 2>/dev/null || true
kubectl delete -f "$ROOT/deploy/nitro-placeholder.yaml" \
  --ignore-not-found 2>/dev/null || true

# ── 4. Delete application manifests ──────────────────────────────────────────

echo "--- [4/5] delete application manifests ---"
kubectl delete -f "$ROOT/deploy/e2e-latency-canary.yaml"  --ignore-not-found 2>/dev/null || true
kubectl delete -f "$ROOT/deploy/vsock-proxy-service.yaml" --ignore-not-found 2>/dev/null || true
kubectl delete -f "$ROOT/deploy/daemonset-enclave.yaml"   --ignore-not-found 2>/dev/null || true
kubectl delete -f "$ROOT/deploy/otel-collector.yaml"      --ignore-not-found 2>/dev/null || true
kubectl delete -f "$ROOT/deploy/rbac.yaml"                --ignore-not-found 2>/dev/null || true
kubectl delete -f "$ROOT/deploy/namespace.yaml"           --ignore-not-found 2>/dev/null || true

# ── 5. Terraform destroy ──────────────────────────────────────────────────────

echo "--- [5/5] terraform destroy ---"
cd "$ROOT/terraform"
terraform destroy -auto-approve -input=false -var-file="$TFVARS"

echo ""
echo "=== Destroy complete ==="
echo "Run 'terraform show' to confirm no resources remain."
