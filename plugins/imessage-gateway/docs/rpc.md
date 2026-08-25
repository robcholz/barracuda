# IMessage Gateway RPC API

The IMessage Gateway Component exposes outbound delivery independently from
any Workflow, Agent, or concrete IM provider. All RPCs select their destination
with `GatewayRoute` and return the provider-assigned identifier in
`GatewaySendReceipt`.

## `gateway.send`

Sends one bounded complete text message. This is the simple and dynamic API.

- Address: `gateway.send`
- Request: unary `GatewaySendRequest`
- Response: unary `GatewaySendReceipt`
- Method error: `GatewaySendError`
- Dynamic: yes (`call_json` is supported)

`GatewaySendRequest` contains bounded `channel`, `conversation`, `thread`,
`reply_to`, and `text` fields. Empty `thread` and `reply_to` values mean absent.
Use `GatewaySendRequest::new` or `GatewaySendRequest::with_reply_to` for typed
calls.

### Response

`GatewaySendReceipt` contains `message_id`, the identifier assigned by the
selected provider.

### Errors

| Variant | Meaning |
| --- | --- |
| `InvalidRequest` | A request field is invalid. |
| `UnknownChannel` | `route.channel` does not name a registered `MessageChannel`. |
| `Delivery` | The selected provider rejected the request or its transport failed. |
| `InvalidReceipt` | The provider returned a message identifier that cannot fit in the Gateway response contract. |

## `gateway.send_stream`

Streams primary text and optional extra content in one ordered delivery. This
is the complete typed API.

- Address: `gateway.send_stream`
- Request: streaming `GatewaySendStreamRequestFrame`
- Response: unary `GatewaySendReceipt`
- Method error: `GatewaySendStreamError`
- Dynamic: no

Each fixed-layout request frame contains `value` and one
`GatewaySendStreamField`. The stream begins with `Channel`, `Conversation`,
optional `Thread`, and optional `ReplyTo`. Content follows as field variants:

- `TextMore` / `TextComplete`
- `ReasoningMore` / `ReasoningComplete`
- `EffectResultMore` / `EffectResultComplete`
- `NoticeMore` / `NoticeComplete`
- `EventMore` / `EventComplete`
- structured tool fields from `ToolResultStart` through `ToolResultEnd`

The handler reads only the metadata prefix, then hands the remaining live
stream to the selected provider. `frames_from_gateway_send_stream` encodes a
logical buffered value; `frames_from_gateway_stream_frame` incrementally
encodes one content frame.

Plain providers use the default projection, which consumes all frames and
delivers only `Text`. Rich providers override `MessageChannel::send_stream` to
render extra frames.

Errors are `InvalidRequest`, `UnknownChannel`, `Delivery`, and
`InvalidReceipt`, with the same routing and receipt meanings as `gateway.send`.

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
