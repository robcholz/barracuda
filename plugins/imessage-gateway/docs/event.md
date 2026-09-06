# IMessage Gateway JSON Events

## `gateway.message.received`

Providers publish a typed `GatewayInboundMessage` through `IMessageGateway`.
The Gateway emits one or more bounded JSON Events with this stable Event ID.

A short message is one terminal document:

```json
{"stream_id":21,"sequence":0,"phase":"complete","terminal":true,"route":{"channel":"telegram","conversation_id":"chat-42","thread_id":"topic-7"},"message_id":"message-100","text":"hello"}
```

If that document would exceed the available Event-input lane, the Plugin emits:

1. `phase: "start"`, sequence 0, `terminal: false`, with route and message ID.
2. One or more `phase: "chunk"` documents with consecutive sequences,
   `terminal: false`, and bounded text.
3. One `phase: "finish"` document with the next sequence and
   `terminal: true`.

The numeric `stream_id` correlates every document produced from one inbound
message. Event emission awaits Event Router acceptance one document at a time.

Text has no independent total limit and may span any number of chunks. Route
metadata and message ID must fit the `start` document. The provider ingress
queue is bounded to 16 messages by the Plugin.

## Outbound terminal Events

`gateway.send_stream.finished` and `gateway.send_media.finished` each emit
exactly once after an accepted stream reaches provider completion or failure.
Their documents contain `stream_id`, the accepted finish `sequence`, and either
`outcome: "completed"` with `message_id` or `outcome: "failed"` with `error`.
Each terminal document uses the complete Event lane.
