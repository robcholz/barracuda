# Gateway–Agent Bridge Component

Drives the Agent from inbound gateway messages and delivers the Agent's turn
back to the gateway as ordinary IM messages. It keeps IM providers fully
decoupled from the Agent: nothing agent-specific crosses the gateway boundary.
Rich content (reasoning, tool results, notices) is carried as generic
[`gateway::MessageKind`] roles on ordinary `gateway.send` messages.

The Component is constructed with an already-created Agent session identifier
and the [`GatewayRoute`] its replies are delivered to. The host creates the
session through the Agent runtime before loading the Component.

## Provides

### `bridge.handle`

- Input: streaming `GatewayEventFrame` (the `gateway.message.received` Event
  message). Registered as the ingress step of the gateway Workflow.
- Output: unary `()`.
- Method error: `BridgeHandleError`
  - `InvalidMessage`: the inbound frames did not decode to a message.
  - `AppendFailed`: the bound Agent session rejected the appended text.

The handler decodes the `GatewayInboundMessage`, records its message id as the
next reply's `reply_to`, and appends the (capacity-fitted) text to the bound
session via `session.append`. The Agent turn it triggers is observed by the
outbound pump, not by this RPC.

## Consumes

- `session.open` — the outbound pump opens the bound session once and
  reads its `SessionEventDto` stream for the Component's lifetime.
- `session.append` — used by `bridge.handle` to enqueue inbound text.
- `gateway.send` — used by the outbound pump to deliver each mapped message.

## Event mapping

Per Agent turn, session events are mapped onto `gateway.send` messages:

| Session event | Gateway message |
| --- | --- |
| reasoning delta/end | one `reasoning` message per reasoning block |
| output / effect-output delta/end | one `reply` message per output block (threads `reply_to`) |
| tool result | one `tool` message: `"<name>: ok\|failed"` |
| turn/session error | one `notice` message: `"error: …"` |
| input requested | one `notice` message |

## Lifecycle

`run` opens the bound session and pumps events until the stream closes, then
stays pending so the Component remains resident. The inbound handler waits for
the pump to open the session before appending, so `session.append` always finds
the session lease.
