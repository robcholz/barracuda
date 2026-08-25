# IMessage Gateway Events

The IMessage Gateway Component emits normalized inbound provider messages. Its
Event contract belongs to Gateway and contains no Agent or adapter types.

## `gateway.message.received`

- Event ID: `gateway.message.received`
- Message type: `GatewayEventFrame`
- Cardinality: streaming

### Logical message

`GatewayInboundMessage` contains:

| Field | Type | Meaning |
| --- | --- | --- |
| `route` | `GatewayRoute` | Provider channel, conversation, and optional thread that originated the message. |
| `message_id` | `String` | Provider-assigned identifier of the inbound message. |
| `text` | `String` | Complete user-visible message text. |

Each frame contains one semantic `GatewayEventField` and one NUL-terminated
UTF-8 `GatewayText`. Route fields and `message_id` each occupy one frame.
Message text uses `TextMore` frames followed by exactly one `TextComplete`
frame. Consumers reconstruct the logical message with
`gateway_event_from_frames`.

### Emit timing

1. A concrete provider normalizes its inbound message into
   `GatewayInboundMessage`.
2. The provider awaits `IMessageGateway::publish`. Completion means the
   bounded Gateway ingress queue accepted the logical message.
3. `GatewayComponent::run` receives that queued message and encodes its typed
   Event frames.
4. The Component awaits `EventEmitter::emit::<GatewayMessageReceived>`.
5. The Event is emitted when Event Router accepts the complete frame stream.

Ingress queue acceptance and Event Router acceptance are separate points.
Backpressure can occur at either point.

One accepted `GatewayInboundMessage` produces one
`gateway.message.received` Event stream. Workflow matching and downstream RPC
execution happen after emission and do not change the Gateway Event contract.
