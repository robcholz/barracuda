# IMessage Gateway Usage

The IMessage Gateway Component turns provider ingress into an Event and routes
typed outbound RPCs to registered IM providers. The RPC reference lives in
[`rpc.md`](rpc.md), and the Event contract lives in [`event.md`](event.md).

## Quick start

Register the base Plugin before each provider Plugin, then start the complete
Plugin set. The base Plugin owns `GatewayComponent`; provider Plugins register
their channels through its typed capability.

```rust
manager
    .register(&mut router, IMessageGatewayPlugin::new())?;
manager
    .register(&mut router, IMessageWebPlugin::new())?;
manager.start(&mut router)?;
```

`IMessageWebPlugin` depends on `imessage-gateway` and `webserver`. Telegram,
WeChat, and BlueBubble use their matching provider Plugins. An application
registers the configured provider set before calling `PluginManager::start`.

RPC callers use the Event Router's `RpcClient`.

## 1. Register a provider channel

A provider Plugin declares `imessage-gateway` in `DEPENDS_ON`, requires the
capability during registration, and retains its channel registration:

```rust
let gateway = context.require::<IMessageGateway>(IMESSAGE_GATEWAY_PLUGIN_ID)?;
let channel: Rc<dyn MessageChannel> = provider.clone();
let registration = gateway
    .register(channel)
    .map_err(PluginError::registration)?;
context.retain(registration);
```

Dropping the retained registration unregisters the channel during Plugin
unload.

## 2. Publish an inbound message

Build the shared route and publish the normalized message from the concrete
provider:

```rust
let route = GatewayRoute::new("telegram", "chat-42").with_thread("topic-7");

gateway
    .publish(GatewayInboundMessage {
        route,
        message_id: "message-100".into(),
        text: "hello".into(),
    })
    .await?;
```

The bounded ingress queue applies backpressure. The Component subsequently
emits one `gateway.message.received` Event stream.

## 3. Send complete text

Use the unary dynamic API for a bounded complete message:

```rust
let route = GatewayRoute::new("telegram", "chat-42");
let request = GatewaySendRequest::with_reply_to(
    &route,
    "hello from Gateway",
    Some("message-100"),
)?;
let outcome = client.call::<GatewaySend>(request)?.await?;

match outcome {
    Ok(response) => {
        let message_id = response.view()?.message_id()?.to_owned();
        // Store `message_id` when later edits or replies need it.
    }
    Err(error) => {
        // Handle GatewaySendError.
    }
}
```

The route selects the provider and conversation. `reply_to` selects the
existing provider message being answered.

## 4. Stream text and extras

Use `GatewaySendStream` when content should render while it is produced or
when rich providers need reasoning, effect, notice, event, or structured tool
frames. The RPC input starts with route metadata and continues with content
frames. `frames_from_gateway_send_stream` is convenient for buffered callers;
live mappers can emit `GatewaySendStreamRequestFrame` values incrementally.

Plain IM providers receive only the `Text` projection. Web receives
`message.delta` for text and `message.extra` for every extra frame.

## 5. Send media

Use one `gateway.send_media` contract for files, images, audio, and video. Set
`kind` to choose the provider operation:

```rust
let request = GatewayOutboundMedia {
    route: GatewayRoute::new("telegram", "chat-42"),
    kind: GatewayMediaKind::Image,
    filename: Some("photo.jpg".into()),
    mime_type: Some("image/jpeg".into()),
    caption: Some("latest photo".into()),
    reply_to: None,
    bytes: image_bytes,
};

let input = RpcStream::new(futures_lite::stream::iter(
    frames_from_gateway_send_media(&request)?
        .into_iter()
        .map(Ok),
));
let outcome = client.call::<GatewaySendMedia>(input)?.await?;

match outcome {
    Ok(response) => {
        let message_id = response.view()?.message_id()?.to_owned();
    }
    Err(error) => {
        // Handle GatewaySendMediaError.
    }
}
```

The helper emits metadata first and splits the opaque body into bounded binary
chunks. Input-stream completion completes the media body.

## 6. Consume inbound Events

Use `GatewayMessageReceived` as the Event marker. A Workflow ingress RPC uses
the exact Event message type and streaming cardinality:

```rust
pub struct HandleGatewayMessage;

impl RpcMethod for HandleGatewayMessage {
    const ADDRESS: &'static str = "my_component.handle_gateway_message";
    type Request = <GatewayMessageReceived as Event>::Message;
    type Response = ();
    type Error = HandleGatewayMessageError;
    type Input = Streaming;
    type Output = Unary;
}
```

Collect the request frames and decode the logical message with
`gateway_event_from_frames`. A Workflow whose match is
`gateway.message.received` can call this RPC directly because both contracts
use `GatewayEventFrame`.

## Examples

- `crates/component/tests/component.rs` — typed `gateway.send`,
  `gateway.send_stream`, and `gateway.send_media` calls through Event Router.
- `crates/gateway/tests/gateway.rs` — direct provider routing for text and all
  four media kinds.
- `../imessage-web/crates/provider/tests/web.rs` — Web channel behavior.
- `../imessage-telegram/crates/provider/tests/telegram.rs` — Telegram provider behavior.
- `../imessage-wechat/crates/provider/tests/wechat.rs` — WeChat provider behavior.
- `../imessage-bluebubble/crates/provider/tests/bluebubbles.rs` — BlueBubbles provider behavior.

Run them with:

```console
cargo test -p barracuda-imessage-gateway-component
cargo test -p barracuda-imessage-gateway-plugin
cargo test -p gateway
```

## Constraints

- Use Event Router lane frames of at least 512 bytes when loading
  `GatewayComponent`.
- Route fields, media metadata fields, and returned provider message IDs are
  bounded UTF-8 C strings. A media metadata field holds at most 251 UTF-8
  bytes; a returned message ID holds at most 251 UTF-8 bytes.
- `gateway.send` text is bounded by its unary request field.
- `gateway.send_stream` content and Event bodies span as many typed frames as
  needed.
- Text fields contain UTF-8 and no embedded NUL byte. Media body chunks contain
  opaque bytes.
