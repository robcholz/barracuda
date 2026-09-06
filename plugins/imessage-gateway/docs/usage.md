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

`publish` awaits the bounded ingress queue. The route, message ID, and complete
text are emitted as one Event document. It returns `MessageTooLarge` before
queueing when that encoded document does not fit one Event lane.

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

Call `gateway.send_stream` once for every complete semantic Agent event. Supply
the route with each event. `turn_started` opens the provider stream and
`turn_ended` closes it after being forwarded. The Agent session and sequence are
used directly; Gateway does not assign another stream identity or order. If the
response is `{"error":"busy"}`, yield and retry the same document.

Send `type: "stream_error"` with a payload containing a non-empty `error`
string to terminate an active stream with an error. Other producer correlation
fields may remain in that payload. The same event may be sent without a
preceding `turn_started`; Gateway opens delivery for the supplied route and
`reply_to` and immediately reports the stream failure to the provider.

After `turn_ended` is accepted, match `gateway.send_stream.finished` by
`session` to obtain the provider receipt or failure. The RPC lane is not held
for the lifetime of the provider stream.

`gateway.send_media` keeps the explicit `start`, bounded `chunk`, and `finish`
commands documented in `rpc.md`.

See `rpc.md` for exact JSON and `event.md` for inbound and terminal Event
documents.
