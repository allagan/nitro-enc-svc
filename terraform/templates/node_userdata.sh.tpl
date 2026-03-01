#!/bin/bash
set -euo pipefail

# ---------------------------------------------------------------------------
# Nitro Enclave node bootstrap
#
# This script runs on first boot before the node joins the EKS cluster.
# It configures the nitro-enclaves-allocator so that the enclave runner
# DaemonSet can start the enclave immediately when the pod lands on this node.
# ---------------------------------------------------------------------------

# 1. Configure the Nitro Enclaves allocator with the reserved memory and CPU.
#    These values must be set BEFORE the allocator service starts; changing
#    them later requires a node restart.
cat > /etc/nitro_enclaves/allocator.yaml <<ALLOC
---
memory_mib: ${enclave_memory_mb}
cpu_count: ${enclave_cpu_count}
ALLOC

systemctl enable nitro-enclaves-allocator
systemctl restart nitro-enclaves-allocator

# 2. Bootstrap the EKS node.
#    The node-label tells the DaemonSet node selector where to schedule
#    the enclave runner pod.
/etc/eks/bootstrap.sh "${cluster_name}" \
  --kubelet-extra-args "--node-labels=aws.amazon.com/nitro-enclaves=true"
