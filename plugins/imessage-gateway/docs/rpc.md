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

Start:

```json
{"action":"start","stream_id":"reply-17","sequence":0,"channel":"telegram","conversation_id":"chat-42","reply_to":"message-100"}
```

Chunk:

```json
{"action":"chunk","stream_id":"reply-17","sequence":1,"field":"text","boundary":"more","text":"hel"}
```

Finish:

```json
{"action":"finish","stream_id":"reply-17","sequence":2}
```

An accepted command returns `{"accepted_sequence":1}`. Rejections are
`invalid_request`, `duplicate_stream`, `unknown_stream`, `out_of_order`, or
`busy`. A stream ID uses ASCII letters, digits, `_`, `-`, or `.`. The complete
command must fit the request lane; text has no second field-level cap. The supported content fields
are `text`, `reasoning`, `effect_result`, `notice`, `event`, and the structured
tool-result fields listed in the request schema.

At most four text streams are active, the start queue holds four jobs, and each
stream buffers two commands. On `busy`, retry the same sequence after yielding.

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

A successful stream terminal Event is:

```json
{"stream_id":"reply-17","sequence":2,"outcome":"completed","message_id":"provider-id"}
```

A failure uses `outcome: "failed"` and `error`. Delivery errors are
`invalid_request`, `unknown_channel`, `unsupported`, `authentication`,
`rate_limited`, `delivery`, or `invalid_receipt`. Terminal Events use the
complete Event lane.
