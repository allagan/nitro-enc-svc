# Integration Tests

Integration tests in this directory run against a local mock enclave server
(no real vsock or AWS required).

## Running

```bash
cargo test --workspace
```

Tests that require a vsock-capable host (e.g., the proxy forwarding tests) are
gated behind the `integration` feature flag and skipped in CI unless explicitly
enabled.
