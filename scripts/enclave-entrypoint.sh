#!/bin/sh
# scripts/enclave-entrypoint.sh
#
# Enclave PID-1 wrapper. Execs the nitro-enc-svc binary.
#
# The binary handles the IMDS vsock bridge internally (see start_imds_bridge()
# in main.rs), so no socat or external tool is needed here.

set -e

# The Nitro Enclave kernel does not bring up the loopback interface
# automatically.  The IMDS vsock bridge in the enclave binary binds to
# 127.0.0.1:8004, which requires lo to be UP.  Without this, bind(2) fails
# with EADDRNOTAVAIL and PID 1 exits before the console can connect.
ip link set lo up 2>/dev/null || true

echo "INFO: exec nitro-enc-svc"
exec /usr/local/bin/enclave "$@"
