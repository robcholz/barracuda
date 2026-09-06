# IMessage Gateway JSON RPC API

All three RPCs have visibility `"*"`, unary JSON input/output, and a 512-byte
request ceiling. Malformed JSON or a document that does not match the declared
shape is an Event Router `RpcError`. Stable application rejections are JSON
responses.

## `gateway.send`

- Schema paths: `schemas/rpc/send/request.json` and `response.json`
- Maximum request: 512 bytes
- Maximum response: 512 bytes

Request:

```json
{"channel":"telegram","conversation_id":"chat-42","thread_id":"topic-7","reply_to":"message-100","text":"hello"}
```

Success is `{"message_id":"provider-id"}`. Errors are `invalid_request`,
`unknown_channel`, `unsupported`, `authentication`, `rate_limited`, `delivery`,
or `invalid_receipt`.

There are no independent field limits. The complete encoded request, including
JSON escaping and metadata, must fit the 512-byte RPC lane.

## `gateway.send_stream`

- Schema paths: `schemas/rpc/send_stream/request.json` and `response.json`
- Maximum request: 512 bytes
- Maximum response: 128 bytes
- Terminal Event: `gateway.send_stream.finished`

Each call supplies one complete semantic Agent event plus its delivery route:

```json
{"route":{"channel":"telegram","conversation_id":"chat-42"},"reply_to":"message-100","session":"session-1","sequence":3,"type":"output_delta","payload":{"text":"hello"}}
```

An accepted event returns `{"accepted_sequence":3}`. `turn_started` opens the
provider stream for its `session`; every event is forwarded unchanged; and
`turn_ended` is forwarded before closing the stream. A `stream_error` payload
contains a required non-empty `error` string and may retain producer
correlation fields beside it. It terminates delivery with that error and may
also be the first input for a session, allowing a Workflow failure to open and
immediately fail a provider stream for the original route and `reply_to`.
Rejections are
`invalid_request`, `duplicate_stream`, `unknown_stream`, `out_of_order`, or
`busy`. The Agent `session` is the stream correlation key and its `sequence`
is used directly; Gateway does not add another stream ID, sequence, field, or
chunk boundary. The complete request must fit the 512-byte request lane.

At most four semantic event streams are active, the start queue holds four
jobs, and each stream buffers two events. On `busy`, retry the same event after
yielding.

## `gateway.send_media`

- Schema paths: `schemas/rpc/send_media/request.json` and `response.json`
- Maximum request: 512 bytes
- Maximum response: 128 bytes
- Terminal Event: `gateway.send_media.finished`

Start supplies route metadata and `kind` (`file`, `image`, `audio`, or
`video`). Chunks contain `content_base64`; finish follows the same sequence
rules as text:

```json
{"action":"start","stream_id":"upload-4","sequence":0,"channel":"telegram","conversation_id":"chat-42","kind":"image","filename":"photo.jpg","mime_type":"image/jpeg"}
```

```json
{"action":"chunk","stream_id":"upload-4","sequence":1,"content_base64":"AAH/gA=="}
```

```json
{"action":"finish","stream_id":"upload-4","sequence":2}
```

Each Base64 command uses the complete request lane. Media has the same
four-active/four-start/two-command bounds and command responses as text. Queued
commands retain their RPC lanes; decoded chunks use inline storage.

## Terminal Events

A successful semantic event stream terminal Event is:

```json
{"session":"session-1","sequence":8,"outcome":"completed","message_id":"provider-id"}
```

A failure uses `outcome: "failed"` and `error`. Delivery errors are
`invalid_request`, `unknown_channel`, `unsupported`, `authentication`,
`rate_limited`, `delivery`, or `invalid_receipt`. Terminal Events use the
complete Event lane.
