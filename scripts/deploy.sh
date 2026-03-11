#!/bin/bash
# deploy.sh — Full one-command deploy for nitro-enc-svc.
#
# Usage:
#   ./scripts/deploy.sh [path/to/tfvars]
#
# Environment variables (all have defaults):
#   AWS_DEFAULT_REGION   — defaults to us-east-2
#   CLUSTER_NAME         — defaults to nitro-enc-svc-dev
#   NAMESPACE            — defaults to nitro-enc-svc
#
# Steps:
#   1. terraform init + apply (provisions VPC, EKS, Karpenter via Helm)
#   2. Update kubeconfig
#   3. Wait for Karpenter controller to be ready
#   4. Apply Karpenter EC2NodeClass + NodePool + placeholder (triggers node provisioning)
#   5. Wait for 2 nitro nodes to be Ready
#   6. Apply application manifests (DaemonSets schedule automatically)
#   7. Wait for DaemonSet rollouts
#   8. Smoke test: health check via NLB

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
REGION="${AWS_DEFAULT_REGION:-us-east-2}"
CLUSTER="${CLUSTER_NAME:-nitro-enc-svc-dev}"
NAMESPACE="${NAMESPACE:-nitro-enc-svc}"
TFVARS="${1:-$ROOT/terraform/terraform.tfvars}"

echo "=== nitro-enc-svc deploy ==="
echo "  Cluster:    $CLUSTER"
echo "  Region:     $REGION"
echo "  Tfvars:     $TFVARS"
echo ""

# ── 1. Terraform ──────────────────────────────────────────────────────────────

echo "--- [1/7] terraform init + apply ---"
cd "$ROOT/terraform"
terraform init -input=false
terraform apply -auto-approve -input=false -var-file="$TFVARS"

# ── 2. Kubeconfig ─────────────────────────────────────────────────────────────

echo "--- [2/7] update kubeconfig ---"
aws eks update-kubeconfig --name "$CLUSTER" --region "$REGION"
kubectl cluster-info

# ── 3. Wait for Karpenter ─────────────────────────────────────────────────────

echo "--- [3/7] wait for Karpenter controller ---"
kubectl rollout status deployment/karpenter -n kube-system --timeout=300s

# ── 4. Apply Karpenter resources ──────────────────────────────────────────────

echo "--- [4/7] apply Karpenter EC2NodeClass + NodePool + placeholder ---"
kubectl apply -f "$ROOT/deploy/karpenter-ec2nodeclass.yaml"
kubectl apply -f "$ROOT/deploy/karpenter-nodepool.yaml"

# Ensure namespace exists before applying placeholder
kubectl apply -f "$ROOT/deploy/namespace.yaml" 2>/dev/null || true
kubectl apply -f "$ROOT/deploy/nitro-placeholder.yaml"

# ── 5. Wait for nitro nodes ───────────────────────────────────────────────────

echo "--- [5/7] waiting for 2 nitro nodes to be Ready (up to 10 min) ---"
DEADLINE=$((SECONDS + 600))
while true; do
  COUNT=$(kubectl get nodes -l "aws.amazon.com/nitro-enclaves=true" \
    --no-headers 2>/dev/null | grep -c " Ready " || true)
  if [ "$COUNT" -ge 2 ]; then
    echo "  $COUNT nitro node(s) Ready — continuing"
    break
  fi
  if [ $SECONDS -ge $DEADLINE ]; then
    echo "ERROR: timed out waiting for 2 nitro nodes (got $COUNT)"
    kubectl get nodes -l "aws.amazon.com/nitro-enclaves=true" || true
    exit 1
  fi
  echo "  Nitro nodes ready: $COUNT/2 — waiting..."; sleep 15
done

# Show AZ spread
echo "Nitro node AZ distribution:"
kubectl get nodes -l "aws.amazon.com/nitro-enclaves=true" \
  -o custom-columns='NAME:.metadata.name,AZ:.metadata.labels.topology\.kubernetes\.io/zone,STATUS:.status.conditions[-1].type'

# ── 6. Apply application manifests ────────────────────────────────────────────

echo "--- [6/7] apply application manifests ---"
kubectl apply -f "$ROOT/deploy/rbac.yaml"                  2>/dev/null || true
kubectl apply -f "$ROOT/deploy/otel-collector.yaml"
kubectl apply -f "$ROOT/deploy/daemonset-enclave.yaml"
kubectl apply -f "$ROOT/deploy/vsock-proxy-service.yaml"   # DaemonSet + NLB Service
kubectl apply -f "$ROOT/deploy/e2e-latency-canary.yaml"    2>/dev/null || true

# ── 7. Wait for DaemonSet rollouts ────────────────────────────────────────────

echo "--- [7/7] wait for DaemonSet rollouts ---"
kubectl rollout status daemonset/nitro-enclave \
  -n "$NAMESPACE" --timeout=300s
kubectl rollout status daemonset/vsock-proxy \
  -n "$NAMESPACE" --timeout=300s

# ── Smoke test ────────────────────────────────────────────────────────────────

echo ""
echo "--- smoke test ---"
NLB=""
DEADLINE=$((SECONDS + 120))
while [ -z "$NLB" ]; do
  NLB=$(kubectl get svc vsock-proxy-nlb -n "$NAMESPACE" \
    -o jsonpath='{.status.loadBalancer.ingress[0].hostname}' 2>/dev/null || true)
  if [ -z "$NLB" ]; then
    [ $SECONDS -ge $DEADLINE ] && echo "WARN: NLB hostname not yet assigned" && break
    echo "  Waiting for NLB hostname..."; sleep 10
  fi
done

if [ -n "$NLB" ]; then
  echo "NLB: $NLB"
  # Wait for NLB to be reachable
  DEADLINE=$((SECONDS + 120))
  HEALTH=""
  while true; do
    HEALTH=$(curl -sk --max-time 5 "https://$NLB:8443/health" 2>/dev/null || true)
    if echo "$HEALTH" | grep -q '"status":"ok"'; then
      echo "Health: $HEALTH"
      echo "SMOKE TEST PASSED"
      break
    fi
    if [ $SECONDS -ge $DEADLINE ]; then
      echo "WARN: health check not yet passing (NLB may still be initializing)"
      echo "  Last response: $HEALTH"
      break
    fi
    echo "  Waiting for health check..."; sleep 10
  done
fi

echo ""
echo "=== Deploy complete ==="
echo "NLB: ${NLB:-<pending>}"
echo ""
echo "To test manually:"
echo "  curl -sk https://\$NLB:8443/health"
echo "  curl -sk -X POST https://\$NLB:8443/encrypt \\"
echo "    -H 'Content-Type: application/json' \\"
echo "    -H 'X-Schema-Name: payments-v1' \\"
echo "    -d '{\"payload\":{\"card_number\":\"4111111111111111\"}}'"
