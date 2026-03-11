# Deployment Guide — nitro-enc-svc

## Architecture

```
NLB (internal, externalTrafficPolicy: Local)
  │
  ├── Node A (us-east-2a)
  │     vsock-proxy DaemonSet Pod → Enclave DaemonSet Pod (vsock CID 16)
  │                                  ↑ aws-vsock-proxy systemd units
  └── Node B (us-east-2b)
        vsock-proxy DaemonSet Pod → Enclave DaemonSet Pod (vsock CID 16)
                                     ↑ aws-vsock-proxy systemd units

Karpenter NodePool → EC2NodeClass (nitroEnclavesEnabled=true)
  Provisions c5.xlarge instances in us-east-2a + us-east-2b.
  Disruption disabled — nodes are long-lived DaemonSet hosts.

nitro-placeholder Deployment (2 replicas, topologySpreadConstraints)
  Signals Karpenter to maintain 2 nodes in 2 AZs.
```

### Traffic flow

1. Client → NLB (port 8443)
2. NLB → vsock-proxy pod on Node X (externalTrafficPolicy: Local)
3. vsock-proxy → Enclave on Node X via vsock (CID 16, port 443)
4. Enclave decrypts TLS, processes JSON, encrypts PII fields, returns response
5. Response reverses back to client

Because `externalTrafficPolicy: Local` is set on the NLB Service, traffic that lands on a node always goes to the vsock-proxy pod on that same node, which connects to the enclave on that node via vsock. No cross-node hop is possible.

### Node bootstrap (userdata)

Before each node joins EKS, the userdata script:
1. Installs `aws-nitro-enclaves-cli`
2. Configures `nitro-enclaves-allocator` (2048 MiB, 2 vCPUs)
3. Creates `/etc/nitro_enclaves/vsock-proxy.yaml` allowlist
4. Creates and starts 6 systemd units (`nitro-vsock-{kms,sm,s3,imds,otlp,logs}`)

This replaces the previous manual `vsock-proxy` process setup on the parent EC2.

---

## One-command Deploy

```bash
./scripts/deploy.sh [path/to/terraform.tfvars]
```

Default tfvars: `terraform/terraform.tfvars`

Steps performed:
1. `terraform init` + `terraform apply` — VPC, EKS, IAM, KMS, ECR, Karpenter (Helm)
2. `aws eks update-kubeconfig`
3. Wait for Karpenter controller (kube-system)
4. Apply `karpenter-ec2nodeclass.yaml` + `karpenter-nodepool.yaml` + `nitro-placeholder.yaml`
5. Wait for 2 nitro nodes Ready (up to 10 min)
6. Apply application DaemonSets + NLB Service + OTEL collector + canary
7. Wait for DaemonSet rollouts
8. Smoke test health check via NLB

---

## One-command Destroy

```bash
./scripts/destroy.sh [path/to/terraform.tfvars]
```

Steps performed:
1. Delete Karpenter NodePool → triggers graceful node termination
2. Wait for nitro nodes to terminate
3. Delete EC2NodeClass + placeholder
4. Delete application manifests (DaemonSets, Service, OTEL, canary)
5. `terraform destroy`

---

## CodePipeline

The pipeline has 4 stages:

| Stage | Action | Description |
|---|---|---|
| Source | GitHub | Triggered on push to `main` |
| Build | CodeBuild `nitro-enc-svc-dev` | Compiles, builds EIF, extracts PCR0 |
| Approve | Manual | Review PCR0 in `enclave/build-summary.json` |
| DeployAndTest | CodeBuild `nitro-enc-svc-dev-test` | Health + encrypt + ab load test |

### Starting a pipeline run

```bash
aws codepipeline start-pipeline-execution --name nitro-enc-svc-dev
```

### Approving PCR0

```bash
# Get approval token
TOKEN=$(aws codepipeline get-pipeline-state --name nitro-enc-svc-dev \
  --query 'stageStates[?stageName==`Approve`].actionStates[0].latestExecution.token' \
  --output text)

# Approve
aws codepipeline put-approval-result \
  --pipeline-name nitro-enc-svc-dev \
  --stage-name Approve \
  --action-name ReviewPCR0 \
  --result summary="PCR0 verified",status=Approved \
  --token "$TOKEN"
```

---

## Karpenter Node Management

### Check nitro nodes

```bash
kubectl get nodes -l aws.amazon.com/nitro-enclaves=true \
  -o custom-columns='NAME:.metadata.name,AZ:.metadata.labels.topology\.kubernetes\.io/zone,STATUS:.status.conditions[-1].type'
```

### Check Karpenter NodePool

```bash
kubectl get nodepool nitro
kubectl get ec2nodeclass nitro
```

### Karpenter logs

```bash
kubectl logs -n kube-system -l app.kubernetes.io/name=karpenter -f
```

---

## Smoke Tests

From any host in the VPC (not the nitro node itself — NLB hairpin limitation):

```bash
NLB=$(kubectl get svc vsock-proxy-nlb -n nitro-enc-svc \
  -o jsonpath='{.status.loadBalancer.ingress[0].hostname}')

# Health
curl -sk "https://$NLB:8443/health"

# Encrypt
curl -sk -X POST "https://$NLB:8443/encrypt" \
  -H "Content-Type: application/json" \
  -H "X-Schema-Name: payments-v1" \
  -d '{"payload":{"card_number":"4111111111111111","card_holder_name":"Jane Smith"}}'

# Load test (keep-alive, c=10, n=1000)
echo '{"payload":{"card_number":"4111111111111111"}}' > /tmp/body.json
ab -k -c 10 -n 1000 -p /tmp/body.json -T application/json \
  -H "X-Schema-Name: payments-v1" "https://$NLB:8443/encrypt"
```

---

## Troubleshooting

### Karpenter not provisioning nodes

```bash
# Check Karpenter logs for errors
kubectl logs -n kube-system -l app.kubernetes.io/name=karpenter | grep -i error

# Check NodePool events
kubectl describe nodepool nitro

# Verify EC2NodeClass is ready
kubectl describe ec2nodeclass nitro
```

Common causes:
- IAM role `nitro-enc-svc-dev-karpenter-controller` missing permissions
- Pod Identity association not applied yet — wait for `eks-pod-identity-agent` addon
- Subnet/SG selector tags don't match — verify tags on private subnets

### Nodes join but DaemonSets don't schedule

```bash
kubectl describe daemonset nitro-enclave -n nitro-enc-svc
kubectl get events -n nitro-enc-svc --sort-by='.lastTimestamp'
```

Verify the node label is set: `kubectl get nodes --show-labels | grep nitro-enclaves`

### vsock-proxy systemd units not running on node

```bash
# SSH to node (or use SSM Session Manager)
systemctl status nitro-vsock-kms nitro-vsock-sm nitro-vsock-s3 \
  nitro-vsock-imds nitro-vsock-otlp nitro-vsock-logs
journalctl -u nitro-vsock-kms -n 50
```

### Enclave fails to start (DEK unavailable)

1. Check vsock-proxy units are running (see above)
2. Verify KMS key policy allows decryption from the enclave node role
3. Check Secrets Manager ARN in the DaemonSet env vars
4. Check enclave logs: `kubectl logs -n nitro-enc-svc -l app=nitro-enclave`

### NLB health check failing

```bash
# Check which nodes the NLB is targeting
kubectl get svc vsock-proxy-nlb -n nitro-enc-svc -o yaml

# Verify vsock-proxy pods are Running on nitro nodes
kubectl get pods -n nitro-enc-svc -o wide -l app=vsock-proxy
```

NLB health checks go to the `healthCheckNodePort` (kube-proxy). If pods are Running but health check fails, verify `externalTrafficPolicy: Local` is set and the node has at least one Ready pod.

---

## Files Reference

| File | Purpose |
|---|---|
| `scripts/deploy.sh` | One-command full deploy |
| `scripts/destroy.sh` | One-command full teardown |
| `deploy/karpenter-ec2nodeclass.yaml` | Karpenter EC2NodeClass (nitroEnclavesEnabled) |
| `deploy/karpenter-nodepool.yaml` | Karpenter NodePool (c5.xlarge, 2-AZ, disruption=0) |
| `deploy/nitro-placeholder.yaml` | Keeps 2 nodes alive via topology spread |
| `deploy/vsock-proxy-service.yaml` | vsock-proxy DaemonSet + NLB Service |
| `deploy/daemonset-enclave.yaml` | Nitro Enclave runner DaemonSet |
| `buildspec-test.yml` | Post-deploy smoke tests (CodeBuild Stage 4) |
| `terraform/templates/node_userdata.sh.tpl` | Node bootstrap script (allocator + vsock-proxy systemd) |
