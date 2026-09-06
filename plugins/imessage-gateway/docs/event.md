# IMessage Gateway JSON Events

## `gateway.message.received`

Providers publish a typed `GatewayInboundMessage` through `IMessageGateway`.
The Gateway emits exactly one bounded JSON Event with this stable Event ID:

```json
{"route":{"channel":"telegram","conversation_id":"chat-42","thread_id":"topic-7"},"message_id":"message-100","text":"hello"}
```

The complete encoded document, including route metadata, message ID, text, JSON
escaping, and the Event envelope, must fit one Event Router lane. Provider
ingress returns `MessageTooLarge` before queueing a document that exceeds this
capacity. The provider ingress queue is bounded to 16 messages by the Plugin.

## Outbound terminal Events

`gateway.send_stream.finished` and `gateway.send_media.finished` each emit
exactly once after an accepted stream reaches provider completion or failure.
The semantic `gateway.send_stream.finished` document contains `session`, the
accepted `turn_ended` sequence, and either `outcome: "completed"` with
`message_id` or `outcome: "failed"` with `error`. Media terminal documents keep
their `stream_id`. Each terminal document uses the complete Event lane.
