# Message Gateway Usage

The Message Gateway Component turns provider ingress into an Event and routes
typed outbound RPCs to registered IM providers. The RPC reference lives in
[`rpc.md`](rpc.md), and the Event contract lives in [`event.md`](event.md).

## Quick start

Register concrete channels with `MessageGateway`, then load
`GatewayComponent` into an Event Router whose lane frame capacity is at least
512 bytes.

```rust
let mut gateway = gateway::MessageGateway::new();
gateway.register(telegram_channel)?;

let (component, ingress) = GatewayComponent::new(gateway, 16);
router.load(Box::new(component))?;
```

Keep `ingress` with the provider-side receiver. RPC callers use the Event
Router's `RpcClient`.

## 1. Publish an inbound message

Build the shared route and publish the normalized message from the concrete
provider:

```rust
let route = GatewayRoute::new("telegram", "chat-42").with_thread("topic-7");

ingress
    .publish(GatewayInboundMessage {
        route,
        message_id: "message-100".into(),
        text: "hello".into(),
    })
    .await?;
```

The bounded ingress queue applies backpressure. The Component subsequently
emits one `gateway.message.received` Event stream.

## 2. Send text

Build one logical request and use `frames_from_gateway_send` to preserve the
typed frame protocol:

```rust
let request = GatewayOutboundMessage {
    route: GatewayRoute::new("telegram", "chat-42"),
    text: "hello from Gateway".into(),
    reply_to: Some("message-100".into()),
    kind: gateway::MessageKind::Reply,
};

let input = RpcStream::new(futures_lite::stream::iter(
    frames_from_gateway_send(&request)?.into_iter().map(Ok),
));
let outcome = client.call::<GatewaySend>(input)?.await?;

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

## 3. Send media

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

## 4. Consume inbound Events

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

- `crates/component/tests/component.rs` — typed `gateway.send` and
  `gateway.send_media` calls through a running Event Router.
- `crates/gateway/tests/gateway.rs` — direct provider routing for text and all
  four media kinds.

Run them with:

```console
cargo test -p barracuda-message-gateway-component
cargo test -p gateway
```

## Constraints

- Use Event Router lane frames of at least 512 bytes when loading
  `GatewayComponent`.
- Route fields, media metadata fields, and returned provider message IDs are
  bounded UTF-8 C strings. A media metadata field holds at most 251 UTF-8
  bytes; a returned message ID holds at most 251 UTF-8 bytes.
- Text message and Event bodies span as many typed text frames as needed.
- Text fields contain UTF-8 and no embedded NUL byte. Media body chunks contain
  opaque bytes.
