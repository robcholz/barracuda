# Message Gateway Component

The Message Gateway integration is an Event Router `Component`. It normalizes
transport ingress into an Event and exposes outbound delivery as an RPC. It
does not know which Workflow or Agent consumes a message.

Concrete IM providers (Telegram, WeChat, BlueBubbles, Web, and future
providers) integrate directly through `MessageChannel` and `GatewayIngress`.
They do not use an Adapter RPC.

## Emits

### `gateway.message.received`

- Cardinality: streaming
- Message type: `GatewayEventFrame` (64-byte frames, owned by this crate)
- Logical payload: UTF-8 JSON encoded `GatewayInboundMessage`
- Acceptance: `GatewayIngress::publish` only acknowledges queueing. Event
  Router acceptance happens later inside the Component run loop.

`GatewayInboundMessage` carries the channel, conversation and optional thread
route, the provider message ID, and text.

## RPC

### `gateway.send`

- Input: streaming `GatewaySendRequestFrame` values containing one JSON
  `GatewayOutboundMessage`
- Output: unary `()` after the selected `MessageChannel` accepts the send
- Method error: `GatewaySendError`
  - `GatewaySendError::InvalidRequest`: malformed frame stream or JSON
  - `GatewaySendError::Delivery`: unknown channel or provider send failure

The outbound message carries the original route, response text, and an
optional message ID to reply to.

The Event and RPC are Gateway-owned contracts. They do not reference Agent or
any Adapter; protocol conversion belongs to a separate Adapter Component.
`gateway_send_handler` is public and lives beside `GatewaySend`; `component.rs`
only registers it and owns lifecycle state.

## Lifecycle and backpressure

`GatewayComponent::new` returns a cloneable `GatewayIngress` handle. Producers
await the bounded ingress queue, so overload applies backpressure before Event
Router lane capacity is consumed. Dropping every ingress handle leaves the
Component idle until it is unloaded. Event Router owns RPC unregistration.
