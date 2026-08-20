# Gateway–Agent Adapter Component

This Event Router Component owns protocol conversion and correlation between
Gateway-owned contracts and Agent-owned contracts. The dependency direction is:

```text
Gateway contract <- Gateway–Agent Adapter -> Agent contract
```

The Gateway and Agent crates do not reference each other or this adapter. The
adapter depends directly on both endpoint crates and uses their existing Event
and RPC associated types; there is no shared protocol crate and no duplicated
business DTO layer.

Each RPC file exports its public handler constructor beside the method and DTO
definitions. `component.rs` only owns the shared `AdapterRegistry`, registers
those handlers, and clears lifecycle state on unload.

## RPCs

### `adapter.bind_route`

- Input: streaming `BindRouteRequestFrame` encoding one `BindRouteRequest`
- Output: unary `()`
- Method error: `BindRouteError`

This adapter-owned configuration maps a `GatewayRoute` to an existing Agent
`SessionId`. It does not create or open the session. The host remains
responsible for Agent session lifecycle and for keeping `agent.open_session`
connected.

### `adapter.gateway_to_agent`

- Input: the exact message type emitted by `gateway.message.received`
- Output: the exact request type accepted by `session.append`
- Method error: `GatewayToAgentError`

The adapter decodes `GatewayInboundMessage`, resolves its configured
route/session binding, remembers the provider message ID for reply correlation,
and produces `AppendRequestFrame` values. Missing bindings are reported rather
than inventing a session key or allocating a session implicitly.

### `adapter.agent_to_gateway`

- Input: the exact response type returned by `agent.open_session`
- Output: the exact request type accepted by `gateway.send`
- Method error: `AgentToGatewayError`

The adapter consumes complete Agent session events incrementally. It collects
root-visible output and effect-output deltas until `TurnEnded`, then produces a
`GatewayOutboundMessage` for the bound route and replies to the most recent
inbound provider message. Reasoning, tool-result, boundary, and control events
are preserved by Agent but do not become user-visible Gateway messages.

## Emits

This Component emits no Events.

## Lifecycle

Bindings, reply correlation, and partial turn output live only inside the
Component. Unloading clears all three and Event Router unregisters its RPCs.
