# IMessage Gateway Usage

Provider Plugins use the typed capability; Agents and Workflows use JSON.

## Provider registration and ingress

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

`publish` awaits the bounded ingress queue. Text may span any number of Event
documents. Route metadata and the message ID must fit the first Event document.

## Complete JSON send

```rust
let address = RpcAddress::try_from("gateway.send")?;
let response = client
    .call_json(
        &address,
        r#"{"channel":"telegram","conversation_id":"chat-42","text":"hello"}"#,
    )?
    .await?;
```

## Streaming JSON send

Call `gateway.send_stream` or `gateway.send_media` once for `start`, once per
bounded `chunk`, and once for `finish`. Begin at sequence 0 and increment only
after the response echoes `accepted_sequence`. If the response is
`{"error":"busy"}`, yield and retry the same document. Do not advance the
sequence.

After finish is accepted, match the corresponding `.finished` Event by
`stream_id` to obtain the provider receipt or failure. The RPC lane is not held
for the lifetime of the provider stream.

See `rpc.md` for exact JSON and `event.md` for inbound and terminal Event
documents.
