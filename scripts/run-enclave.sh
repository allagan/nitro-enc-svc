#!/usr/bin/env bash
# scripts/run-enclave.sh
#
# Entrypoint for the nitro-enc-svc runner container (DaemonSet pod).
#
# Responsibilities:
#   1. Validate required environment variables.
#   2. Launch the Nitro Enclave via nitro-cli run-enclave.
#   3. Stream enclave console output to stdout (visible via `kubectl logs`).
#   4. Health-monitor the enclave in a loop; exit 1 if it stops running so
#      Kubernetes restarts the pod and relaunches the enclave.
#
# Environment variables consumed by this script (set in daemonset-enclave.yaml):
#   ENCLAVE_MEMORY_MB     — Memory (MB) to allocate to the enclave. Required.
#   ENCLAVE_CPU_COUNT     — vCPUs to allocate to the enclave. Required.
#   ENCLAVE_CID           — Vsock CID for the enclave. Required.
#                           MUST match VSOCK_PROXY_CID baked into the EIF.
#   EIF_PATH              — Path to the EIF file. Default: /enclave/nitro-enc-svc.eif
#   HEALTH_CHECK_INTERVAL — Seconds between health checks. Default: 10

set -euo pipefail

# ---------------------------------------------------------------------------
# 1. Validate required environment variables
# ---------------------------------------------------------------------------
: "${ENCLAVE_MEMORY_MB:?ENCLAVE_MEMORY_MB is required (e.g. 2048)}"
: "${ENCLAVE_CPU_COUNT:?ENCLAVE_CPU_COUNT is required (e.g. 2)}"
: "${ENCLAVE_CID:?ENCLAVE_CID is required (e.g. 16)}"

EIF_PATH="${EIF_PATH:-/enclave/nitro-enc-svc.eif}"
HEALTH_CHECK_INTERVAL="${HEALTH_CHECK_INTERVAL:-10}"

if [[ ! -f "${EIF_PATH}" ]]; then
    echo "ERROR: EIF not found at ${EIF_PATH}" >&2
    exit 1
fi

echo "INFO: Starting Nitro Enclave"
echo "INFO:   EIF path:              ${EIF_PATH}"
echo "INFO:   Memory (MB):           ${ENCLAVE_MEMORY_MB}"
echo "INFO:   CPU count:             ${ENCLAVE_CPU_COUNT}"
echo "INFO:   CID:                   ${ENCLAVE_CID}"
echo "INFO:   Health check interval: ${HEALTH_CHECK_INTERVAL}s"

# ---------------------------------------------------------------------------
# 2. Launch the enclave
#    --enclave-cid pins the vsock CID so vsock-proxy sidecars can connect to
#    a stable, well-known address across enclave restarts on this node.
# ---------------------------------------------------------------------------
RUN_OUTPUT=$(nitro-cli run-enclave \
    --eif-path   "${EIF_PATH}" \
    --memory     "${ENCLAVE_MEMORY_MB}" \
    --cpu-count  "${ENCLAVE_CPU_COUNT}" \
    --enclave-cid "${ENCLAVE_CID}" \
    2>&1) || {
    echo "ERROR: nitro-cli run-enclave failed:" >&2
    echo "${RUN_OUTPUT}" >&2
    exit 1
}

echo "INFO: nitro-cli run-enclave output: ${RUN_OUTPUT}"

ENCLAVE_ID=$(echo "${RUN_OUTPUT}" | jq -r '.EnclaveID // empty')

if [[ -z "${ENCLAVE_ID}" ]]; then
    echo "ERROR: could not extract EnclaveID from nitro-cli output" >&2
    echo "${RUN_OUTPUT}" >&2
    exit 1
fi

echo "INFO: Enclave launched. EnclaveID=${ENCLAVE_ID}"

# ---------------------------------------------------------------------------
# 3. Cleanup handler — terminate enclave when the runner container exits
# ---------------------------------------------------------------------------
cleanup() {
    echo "INFO: Shutting down — terminating enclave ${ENCLAVE_ID}"
    kill "${CONSOLE_PID:-}" 2>/dev/null || true
    nitro-cli terminate-enclave --enclave-id "${ENCLAVE_ID}" 2>/dev/null || true
}
trap cleanup EXIT TERM INT

# ---------------------------------------------------------------------------
# 4. Stream enclave console to stdout in the background
#    Enclave application logs (tracing output) appear here, visible via:
#      kubectl logs -n nitro-enc-svc -l app=nitro-enclave -f
# ---------------------------------------------------------------------------
nitro-cli console --enclave-id "${ENCLAVE_ID}" &
CONSOLE_PID=$!

# ---------------------------------------------------------------------------
# 5. Health monitoring loop
#    Poll nitro-cli every HEALTH_CHECK_INTERVAL seconds.
#    Exit 1 if the enclave leaves the RUNNING state so Kubernetes restarts
#    this pod and relaunches the enclave automatically.
# ---------------------------------------------------------------------------
echo "INFO: Entering health monitoring loop (interval=${HEALTH_CHECK_INTERVAL}s)"

while true; do
    sleep "${HEALTH_CHECK_INTERVAL}"

    DESCRIBE=$(nitro-cli describe-enclaves 2>&1)
    STATE=$(echo "${DESCRIBE}" | \
        jq -r --arg id "${ENCLAVE_ID}" \
        '.[] | select(.EnclaveID == $id) | .State // empty')

    if [[ -z "${STATE}" ]]; then
        echo "ERROR: Enclave ${ENCLAVE_ID} not found in describe-enclaves output" >&2
        echo "${DESCRIBE}" >&2
        exit 1
    fi

    if [[ "${STATE}" != "RUNNING" ]]; then
        echo "ERROR: Enclave ${ENCLAVE_ID} state is '${STATE}' (expected RUNNING)" >&2
        exit 1
    fi

    echo "INFO: Enclave ${ENCLAVE_ID} is RUNNING"
done
