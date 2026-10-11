# IMessage Gateway Workflow Events

Providers publish `GatewayInboundMessage` through `IMessageGateway`. The
Gateway emits the complete value as `gateway.message.received`:

```json
{"route":{"channel":"telegram","conversation_id":"chat-42","thread_id":"topic-7"},"message_id":"message-100","text":"hello"}
```

`thread_id` is omitted when the provider message has no thread selection; it
is never serialized as `null`.

A message with files carries `attachments`, references to files the Gateway
stored; the bytes stay in the files. `text` is then empty when the user sent
only files:

```json
{"route":{"channel":"telegram","conversation_id":"chat-42"},"message_id":"message-101","text":"","attachments":[{"kind":"image","path":"/workspace/cache/gateway/telegram/1a2b3c4d-0.jpg","mime_type":"image/jpeg","filename":"photo.jpg","size":48213}]}
```

| Field | Value |
| --- | --- |
| `kind` | `file`, `image`, `audio`, or `video` |
| `path` | the file in every Plugin's filesystem view, below `/workspace/cache/gateway/<channel>/` |
| `mime_type` | the type the channel or the download reported, else `application/octet-stream` |
| `filename` | the sender's name for the file; omitted when it had none |
| `size` | bytes, at most the `attachment-max-bytes` setting |

`attachments` is omitted when the message has none. The files live in
`/workspace/cache`, so they may vanish, for example on restart; a reader that
finds a path gone treats the attachment as unavailable.

Schema: `schemas/event/gateway.message.received.json`. No Topic. Emitted once
per message a channel publishes; a message whose text is a session command
becomes `gateway.control.received` instead, and its attachments are dropped.

`gateway.send_stream.finished` and `gateway.send_media.finished` are emitted
after provider completion or failure. Successful stream delivery contains its
correlation key, accepted terminal sequence, `outcome: "completed"`, and the
provider `message_id`; failures use `outcome: "failed"` and a stable `error`.
