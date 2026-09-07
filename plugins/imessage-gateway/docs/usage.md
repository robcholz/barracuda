# IMessage Gateway Usage

## 1. Register a provider and publish inbound messages

```rust
let gateway = context.require::<IMessageGateway>("imessage-gateway")?;
let registration = gateway
    .register(provider)
    .map_err(PluginError::registration)?;
context.retain(registration);

gateway
    .publish(GatewayInboundMessage {
        route: GatewayRoute::new("telegram", "chat-42"),
        message_id: "message-100".into(),
        text: "hello".into(),
    })
    .await?;
```

Publishing emits one `gateway.message.received` Workflow Event.

## 2. Send one complete message

```rust
let receipt = gateway
    .send(GatewaySendRequest {
        channel: "telegram".into(),
        conversation_id: "chat-42".into(),
        thread_id: None,
        reply_to: None,
        text: "hello".into(),
    })
    .await?;
```

The same operation is available to Workflows as `gateway.send` and to Agents
as `gateway_send`.

## 3. Forward semantic Agent events

Call `send_stream` for each complete event. `turn_started` opens a provider
stream; `turn_ended` closes it after delivery.

```rust
gateway.send_stream(GatewaySendStreamRequest {
    route: GatewayRoute::new("telegram", "chat-42"),
    reply_to: None,
    session: "session-1".into(),
    sequence: 0,
    event_type: "turn_started".into(),
    payload: serde_json::json!({}),
})?;
```

Match `gateway.send_stream.finished` by `session` to receive the provider
receipt or delivery error. Retry the same sequence after yielding when the
operation returns `busy`.

## 4. Send streamed media

Use `GatewaySendMediaRequest::Start`, one or more `Chunk` commands containing
canonical base64, and `Finish`. Match `gateway.send_media.finished` by
`stream_id` for the terminal result. The same commands are exposed as the
`gateway.send_media` Workflow Action and `gateway_send_media` Agent tool.

See [action.md](action.md) for the JSON contracts and [event.md](event.md) for
Workflow Event documents.
