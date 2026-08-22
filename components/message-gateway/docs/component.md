# Message Gateway Component

The Message Gateway integration is an Event Router `Component`. Concrete IM
providers connect directly through `MessageChannel` and `GatewayIngress`.
Gateway contracts remain independent from Workflows, Agents, and adapters.

The Component provides:

- `gateway.send` for outbound text.
- `gateway.send_media` for outbound files, images, audio, and video.
- `gateway.message.received` for normalized inbound text messages.

Both outbound RPCs use the same `GatewayRoute` and return the same
`GatewaySendReceipt`. Their reusable public handlers live beside their
`RpcMethod` definitions. `component.rs` contains registration and lifecycle
state.

Read the caller-facing documents:

- [`rpc.md`](rpc.md) — RPC addresses, request/response contracts, and every
  method error.
- [`event.md`](event.md) — Event payload, cardinality, and exact emit timing.
- [`usage.md`](usage.md) — loading the Component, publishing ingress, and
  making typed calls.
