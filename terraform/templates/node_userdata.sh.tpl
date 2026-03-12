#!/bin/bash
set -euo pipefail

# ---------------------------------------------------------------------------
# Nitro Enclave node bootstrap
#
# This script runs on first boot before the node joins the EKS cluster.
# It installs the Nitro Enclaves CLI, configures the nitro-enclaves-allocator,
# sets up aws-vsock-proxy systemd units for each AWS endpoint, optionally
# installs aws-nitro-enclaves-acm (p7_proxy) for ACM certificate delivery,
# then joins the cluster.
#
# Template variables:
#   cluster_name      — EKS cluster name for bootstrap
#   enclave_memory_mb — MiB to reserve for the enclave
#   enclave_cpu_count — vCPUs to reserve for the enclave
#   aws_region        — AWS region (e.g. us-east-2)
#   acm_cert_arn      — ACM certificate ARN (empty = skip ACM setup)
# ---------------------------------------------------------------------------

# 1. Install Nitro Enclaves CLI.
#    amazon-linux-extras is required on AL2 to enable the extras topic first.
amazon-linux-extras install -y aws-nitro-enclaves-cli
yum install -y aws-nitro-enclaves-cli-devel

# 2. Configure the Nitro Enclaves allocator with the reserved memory and CPU.
#    These values must be set BEFORE the allocator service starts; changing
#    them later requires a node restart.
cat > /etc/nitro_enclaves/allocator.yaml <<ALLOC
---
memory_mib: ${enclave_memory_mb}
cpu_count: ${enclave_cpu_count}
ALLOC

systemctl enable nitro-enclaves-allocator
systemctl restart nitro-enclaves-allocator

# 3. Configure aws-vsock-proxy allowlist.
#    Lists every remote host/port the enclave is permitted to reach via vsock.
cat > /etc/nitro_enclaves/vsock-proxy.yaml <<ALLOWLIST
---
allowlist:
  - {address: kms.${aws_region}.amazonaws.com, port: 443}
  - {address: secretsmanager.${aws_region}.amazonaws.com, port: 443}
  - {address: s3.${aws_region}.amazonaws.com, port: 443}
  - {address: 169.254.169.254, port: 80}
  - {address: 127.0.0.1, port: 4317}
  - {address: 127.0.0.1, port: 4318}
ALLOWLIST

# 4. Create systemd units for each aws-vsock-proxy endpoint.
#    Format: "<vsock-port> <remote-host> <remote-port>"
declare -A PROXIES
PROXIES["nitro-vsock-kms"]="8001 kms.${aws_region}.amazonaws.com 443"
PROXIES["nitro-vsock-sm"]="8002 secretsmanager.${aws_region}.amazonaws.com 443"
PROXIES["nitro-vsock-s3"]="8003 s3.${aws_region}.amazonaws.com 443"
PROXIES["nitro-vsock-imds"]="8004 169.254.169.254 80"
PROXIES["nitro-vsock-otlp"]="4317 127.0.0.1 4317"
PROXIES["nitro-vsock-logs"]="4318 127.0.0.1 4318"

for NAME in "$${!PROXIES[@]}"; do
  read -r VSOCK_PORT REMOTE_HOST REMOTE_PORT <<< "$${PROXIES[$NAME]}"
  cat > /etc/systemd/system/$NAME.service <<UNIT
[Unit]
Description=Nitro vsock-proxy $NAME
After=network.target

[Service]
ExecStart=/usr/bin/vsock-proxy $VSOCK_PORT $REMOTE_HOST $REMOTE_PORT --config /etc/nitro_enclaves/vsock-proxy.yaml
Restart=always
RestartSec=5

[Install]
WantedBy=multi-user.target
UNIT
  systemctl enable "$NAME"
  systemctl start "$NAME"
done

# 5. (Optional) Install aws-nitro-enclaves-acm when an ACM certificate ARN is
#    configured. This installs p7_proxy which listens on vsock port 9005 and
#    delivers the ACM-managed TLS certificate + attestation-bound private key
#    to acm-ray running inside the enclave.
#
#    The enclave's acm-ray writes the cert and key to:
#      /etc/acm/tls.crt  (TLS_CERT_PATH)
#      /etc/acm/tls.key  (TLS_KEY_PATH)
#
#    Full integration requires the EIF to be rebuilt to include acm-ray.
#    Until then the enclave uses the self-signed cert baked at build time.
ACM_CERT_ARN="${acm_cert_arn}"
if [ -n "$ACM_CERT_ARN" ]; then
  echo "--- [5] Installing aws-nitro-enclaves-acm (ACM cert: $ACM_CERT_ARN) ---"

  # Install the ACM for Nitro Enclaves package (provides p7_proxy).
  amazon-linux-extras install -y aws-nitro-enclaves-acm 2>/dev/null || \
    yum install -y aws-nitro-enclaves-acm

  # Write the acm configuration pointing to the provisioned certificate.
  mkdir -p /etc/nitro_enclaves
  cat > /etc/nitro_enclaves/acm.yaml <<ACM_CFG
---
certificate:
  - arn: $ACM_CERT_ARN
    acm:
      endpoint: https://acm.${aws_region}.amazonaws.com
    # p7_proxy delivers the cert/key to acm-ray inside the enclave
    # over vsock port 9005 (default). acm-ray writes them to the paths
    # below, which map to TLS_CERT_PATH / TLS_KEY_PATH in the enclave.
    enclave:
      vsock_port: 9005
    options:
      # Write cert and key to files so rustls can load them directly.
      cert_path: /etc/acm/tls.crt
      key_path: /etc/acm/tls.key
ACM_CFG

  systemctl enable nitro-enclaves-acm
  systemctl start  nitro-enclaves-acm
  echo "aws-nitro-enclaves-acm started (p7_proxy vsock port 9005)"
else
  echo "--- [5] ACM_CERT_ARN not set — skipping aws-nitro-enclaves-acm (using self-signed cert) ---"
fi

# 6. Bootstrap the EKS node.
#    The node-label tells the DaemonSet node selector where to schedule
#    the enclave runner pod.
/etc/eks/bootstrap.sh "${cluster_name}" \
  --kubelet-extra-args "--node-labels=aws.amazon.com/nitro-enclaves=true"
