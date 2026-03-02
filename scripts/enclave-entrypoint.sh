#!/bin/sh
# scripts/enclave-entrypoint.sh
#
# Enclave PID-1 wrapper.  Starts the IMDS vsock bridge then execs the
# nitro-enc-svc binary.
#
# The bridge (socat) forwards IMDS HTTP traffic from 127.0.0.1:8004 through
# vsock to the parent EC2 (CID from VSOCK_PROXY_CID env var, vsock port 8004),
# where vsock-proxy relays it to the real IMDS endpoint (169.254.169.254:80).
#
# AWS_EC2_METADATA_SERVICE_ENDPOINT=http://127.0.0.1:8004 (baked into the EIF)
# tells the AWS SDK to use the bridge instead of the unreachable link-local
# IMDS address.

set -e

VSOCK_CID="${VSOCK_PROXY_CID:-3}"
IMDS_VSOCK_PORT="${IMDS_VSOCK_PORT:-8004}"
IMDS_LOCAL_PORT="${IMDS_LOCAL_PORT:-8004}"

echo "INFO: starting IMDS vsock bridge (127.0.0.1:${IMDS_LOCAL_PORT} -> vsock(${VSOCK_CID},${IMDS_VSOCK_PORT}))"
socat \
    TCP-LISTEN:${IMDS_LOCAL_PORT},fork,reuseaddr \
    VSOCK-CONNECT:${VSOCK_CID}:${IMDS_VSOCK_PORT} &

echo "INFO: exec nitro-enc-svc"
exec /usr/local/bin/enclave "$@"
