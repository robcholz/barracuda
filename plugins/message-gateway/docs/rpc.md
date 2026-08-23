# Message Gateway RPC API

The Message Gateway Component exposes outbound delivery independently from
any Workflow, Agent, or concrete IM provider. Both RPCs select their
destination with the same `GatewayRoute` and return the provider-assigned
message identifier in `GatewaySendReceipt`.

## `gateway.send`

Sends one text message.

- Address: `gateway.send`
- Request: streaming `GatewaySendRequestFrame`
- Response: unary `GatewaySendReceipt`
- Method error: `GatewaySendError`

### Logical request

`GatewayOutboundMessage` is the caller-facing logical value encoded by
`frames_from_gateway_send`:

| Field | Type | Meaning |
| --- | --- | --- |
| `route` | `GatewayRoute` | Provider channel, conversation, and optional thread. |
| `text` | `String` | Complete user-visible message text. |
| `reply_to` | `Option<String>` | Optional provider message identifier being replied to. |
| `kind` | `MessageKind` | `Reply`, `Reasoning`, `Tool`, or `Notice`. |

Each frame contains a `GatewaySendField`, a `GatewayMessageKind`, and
NUL-terminated UTF-8 `GatewayText`. Route metadata occupies one frame per
field. Message text uses `TextMore` frames followed by exactly one
`TextComplete` frame. The helper preserves UTF-8 character boundaries.

### Response

`GatewaySendReceipt` contains `message_id`, the identifier assigned by the
selected provider.

### Errors

| Variant | Meaning |
| --- | --- |
| `InvalidRequest` | Required fields are absent, a field is duplicated or out of order, message kinds differ between frames, or text is invalid. |
| `UnknownChannel` | `route.channel` does not name a registered `MessageChannel`. |
| `Delivery` | The selected provider rejected the request or its transport failed. |
| `InvalidReceipt` | The provider returned a message identifier that cannot fit in the Gateway response contract. |

## `gateway.send_media`

Sends one file, image, audio, or video through one common RPC.

- Address: `gateway.send_media`
- Request: streaming `GatewaySendMediaRequestFrame`
- Response: unary `GatewaySendReceipt`
- Method error: `GatewaySendMediaError`

### Logical request

`GatewayOutboundMedia` is the caller-facing logical value encoded by
`frames_from_gateway_send_media`:

| Field | Type | Meaning |
| --- | --- | --- |
| `route` | `GatewayRoute` | Provider channel, conversation, and optional thread. |
| `kind` | `GatewayMediaKind` | `File`, `Image`, `Audio`, or `Video`. |
| `filename` | `Option<String>` | Optional provider-visible filename. |
| `mime_type` | `Option<String>` | Optional MIME type. |
| `caption` | `Option<String>` | Optional text rendered with the media. |
| `reply_to` | `Option<String>` | Optional provider message identifier being replied to. |
| `bytes` | `Vec<u8>` | Complete opaque binary media body. |

Each metadata field occupies one typed frame. Metadata frames precede all
`Body` frames. A `Body` frame contains a named `GatewayMediaChunk` with a
`u16` length and opaque bytes. The end of the RPC input stream completes the
media body; there is no separate end payload.

`GatewayMediaKind` selects the matching Gateway operation:

| Kind | Gateway operation |
| --- | --- |
| `File` | `send_file` |
| `Image` | `send_image` |
| `Audio` | `send_audio` |
| `Video` | `send_video` |

### Response

`GatewaySendReceipt` has the same meaning and shape as the response from
`gateway.send`.

### Errors

| Variant | Meaning |
| --- | --- |
| `InvalidRequest` | Required fields are absent, metadata is duplicated, metadata appears after body bytes, media kinds differ between frames, or a frame is invalid. |
| `UnknownChannel` | `route.channel` does not name a registered `MessageChannel`. |
| `Unsupported` | The selected provider does not implement the requested media kind. |
| `Delivery` | The selected provider rejected the request or its transport failed. |
| `InvalidReceipt` | The provider returned a message identifier that cannot fit in the Gateway response contract. |

## Shared destination

Both RPCs use:

```rust
pub struct GatewayRoute {
    pub channel: String,
    pub conversation_id: String,
    pub thread_id: Option<String>,
}
```

`channel` selects a registered provider. `conversation_id` selects the
provider conversation. `thread_id` selects an optional sub-conversation or
topic. `reply_to` is separate from the route and selects an existing message
inside that destination.
