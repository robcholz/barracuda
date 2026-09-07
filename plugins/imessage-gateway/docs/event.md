# IMessage Gateway Workflow Events

Providers publish `GatewayInboundMessage` through `IMessageGateway`. The
Gateway emits the complete value as `gateway.message.received`:

```json
{"route":{"channel":"telegram","conversation_id":"chat-42","thread_id":"topic-7"},"message_id":"message-100","text":"hello"}
```

`thread_id` is omitted when the provider message has no thread selection; it
is never serialized as `null`.

`gateway.send_stream.finished` and `gateway.send_media.finished` are emitted
after provider completion or failure. Successful stream delivery contains its
correlation key, accepted terminal sequence, `outcome: "completed"`, and the
provider `message_id`; failures use `outcome: "failed"` and a stable `error`.
