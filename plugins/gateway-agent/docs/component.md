# Gateway–Agent Component

The Component is a stateful streaming mapper. It owns no provider and exposes
one RPC, `gateway_agent.respond`, whose input is exactly the
`gateway.message.received` Event frame type and whose output is exactly the
`gateway.send_stream` request frame type.

## Workflow

The complete delivery path has two steps:

1. `gateway_agent.respond` decodes the inbound message, finds or lazily creates
   the persistent Agent session for its `GatewayRoute`, appends the user text,
   and maps that turn's `session.open` events to Gateway stream frames.
2. `gateway.send_stream` passes those frames to the provider selected by the
   route prefix.

```text
gateway.message.received
  -> gateway_agent.respond   (stream -> stream)
  -> gateway.send_stream     (stream -> unary receipt)
```

Session creation, `session.open`, and `session.append` are private operations
inside the mapper. They are not Workflow steps.

## `gateway_agent.respond`

- Input: streaming `GatewayEventFrame`
- Output: streaming `GatewaySendStreamRequestFrame`
- Method errors: `InvalidMessage`, `ConversationBusy`, `SessionUnavailable`,
  `AppendFailed`, and `InvalidEvent`

The first output frames are `Channel`, `Conversation`, optional `Thread`, and
`ReplyTo`. Remaining frames are emitted as Agent events arrive; the mapper does
not aggregate a turn before returning it. A dropped consumer releases the
conversation's in-flight guard.

## Event mapping

| Agent event | Gateway stream field |
| --- | --- |
| reasoning delta/end | `Reasoning` (`More` / `Complete`) |
| model output delta/end | `Text` (`More` / `Complete`) |
| effect output delta/end | `EffectResult` (`More` / `Complete`) |
| tool result | structured `ToolResultStart`, call id, name, arguments, output, status, `ToolResultEnd` |
| turn/session error | `Notice` |
| remaining lifecycle/metadata events | `Event` containing the typed event JSON |

Large event values are chunked by the Gateway frame encoder. Plain IM
providers consume the stream but project only `Text`; rich providers such as
Web can render every extra frame.

## State and concurrency

`GatewayRoute` is the conversation key, including channel and optional thread.
Each route owns one persistent Agent session and one long-lived
`session.open` stream. Different routes can run concurrently. A second turn on
the same route while its first response is still streaming receives
`ConversationBusy` rather than interleaving events.
