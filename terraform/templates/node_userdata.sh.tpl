#!/bin/bash
set -euo pipefail

# ---------------------------------------------------------------------------
# Nitro Enclave node bootstrap
#
# This script runs on first boot before the node joins the EKS cluster.
# It installs the Nitro Enclaves CLI (not pre-installed on the EKS AL2 AMI),
# configures the nitro-enclaves-allocator, then joins the cluster.
# ---------------------------------------------------------------------------

# 1. Install Nitro Enclaves CLI.
#    This creates /etc/nitro_enclaves/ and installs the allocator service.
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

# 3. Bootstrap the EKS node.
#    The node-label tells the DaemonSet node selector where to schedule
#    the enclave runner pod.
/etc/eks/bootstrap.sh "${cluster_name}" \
  --kubelet-extra-args "--node-labels=aws.amazon.com/nitro-enclaves=true"
