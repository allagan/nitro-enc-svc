#!/bin/bash
set -euo pipefail

# ---------------------------------------------------------------------------
# Nitro Enclave node bootstrap
#
# This script runs on first boot before the node joins the EKS cluster.
# It installs the Nitro Enclaves CLI, configures the nitro-enclaves-allocator,
# sets up aws-vsock-proxy systemd units for each AWS endpoint, then joins
# the cluster.
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

# 5. Bootstrap the EKS node.
#    The node-label tells the DaemonSet node selector where to schedule
#    the enclave runner pod.
/etc/eks/bootstrap.sh "${cluster_name}" \
  --kubelet-extra-args "--node-labels=aws.amazon.com/nitro-enclaves=true"
