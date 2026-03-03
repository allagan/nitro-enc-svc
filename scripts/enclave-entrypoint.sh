#!/bin/sh
# scripts/enclave-entrypoint.sh
#
# Enclave PID-1 wrapper. Execs the nitro-enc-svc binary.
#
# The binary handles the IMDS vsock bridge internally (see start_imds_bridge()
# in main.rs), so no socat or external tool is needed here.

set -e

echo "INFO: exec nitro-enc-svc"
exec /usr/local/bin/enclave "$@"
