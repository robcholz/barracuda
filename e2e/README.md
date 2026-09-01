# Barracuda end-to-end tests

This package is a host composition root parallel to `apps/`. It constructs the
real `barracuda-system` with two independently owned axes:

- `E2ePlatform` supplies an in-process Embassy IP stack, explicit plaintext TLS,
  and volatile native partitions.
- `E2eBoardHal` supplies deterministic mock GPIO, I2C, SPI, and indicator
  Drivers for physical hardware that is unavailable in CI.

`web_gateway.rs` starts the complete System, configures the Agent through real
HTTP routes, and sends a user message through a TCP WebSocket. Its llm-tape
trajectory verifies that the Agent discovers Event Router groups, loads them
through `tool_load`, and then sees only unary/unary RPCs that are
runtime-dynamic and carry a baked request schema. It invokes the currently
qualifying HTTP and Time RPCs and requires their real results in the next model
request before producing the final Gateway reply.

There is no Barracuda RPC allow-list in the Agent. The generic adapter discovers
Event Router groups and projects each qualifying RPC schema into a hidden tool
group. The test also asserts that typed-only RPCs, streaming RPCs, dynamic RPCs
with `schema == None`, and the removed hardwired tools are absent. E2E tests do
not obtain a `System` RPC client or a Plugin-manager test-control API.

Run it from the workspace root:

```sh
cargo test -p barracuda-e2e
```
