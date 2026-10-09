# IMessage BlueBubble HTTP API

## `GET /api/gateway/bluebubbles/status`

Returns `200` with the shared channel status (see the Gateway's
`docs/plugin.md`). It never returns the password; other methods answer `405`.

```json
{"configured":true,"mode":"send_receive","receive":{"state":"receiving"},"owners":{"count":1},"config":{"server_url":"https://bluebubbles.example.com"},"webhook":{"lost":0,"skipped":2}}
```

- `mode` is `disabled`, `send`, or `send_receive`.
- `receive` is present only in `send_receive`. `state` is `idle`, `starting`,
  `receiving`, or `error` (with `message`). The webhook holds no receive slot,
  so `no_slot` never occurs and `slots` is never present.
- `config` is present while configured and holds the stored server URL.
- `webhook` is present only in `send_receive`. Both counters start at zero at
  boot:
  - `lost`: deliveries the device could not take. These are bodies over
    8 KiB, deliveries arriving while the inbox is full, and messages skipped
    because a catch-up found more than 50 waiting.
  - `skipped`: messages that are not plain text, such as attachments,
    tapbacks, and group events.

## `POST /api/gateway/bluebubbles`

Configures or replaces the BlueBubble message-channel provider. The request body is
a JSON object containing `server_url`, `password`, `use_private_api`, `stream_edit_min_delta_bytes`, and `stream_max_edits`. Fields with provider defaults may be omitted.

Responses:

- `204 No Content`: the provider was configured and, unless the mode is
  `disabled`, registered. Receiving restarts with the new settings. When the
  server URL changed while receiving, the device first deletes its webhook
  from the old server, best effort.
- `400 Bad Request`: the JSON body was invalid or required fields were absent.
- `405 Method Not Allowed`: the endpoint only accepts `POST`.
- `422 Unprocessable Content`, `{"error":"registration_failed"}`: the Gateway
  rejected channel registration. The previous stored configuration is restored
  and no BlueBubbles channel stays registered.
- `500 Internal Server Error`, `{"error":"storage"}`: storage failed.

Credentials are accepted only in the request body and are never included in the
response or logs.

## `POST /api/gateway/bluebubbles/mode`

`{"mode":"disabled"|"send"|"send_receive"}`. Answers `204`. Errors are the
shared ones: `400` `invalid_request` or `unsupported_mode`, `422`
`registration_failed`, `500` `storage`, and `405` for other methods. `409`
`no_slot` never occurs.

Leaving `send_receive` deletes the device's webhook from the BlueBubbles
server before answering. This waits at most 10 s, and a failure is logged.
`disabled` also unregisters the channel from the Gateway.

## `GET` and `POST /api/gateway/bluebubbles/owners`

The shared allowed-accounts endpoint. An owner's `id` is the sender's
BlueBubbles handle address (phone number or e-mail). Owners have no label.
Before the channel is configured both methods answer `409`
`{"error":"not_configured"}`: there is no owner book yet.

## `POST /api/gateway/bluebubbles/hook/<secret>`

The BlueBubbles server posts webhook events here. This device registers the
URL itself; nobody else calls it.

- The path carries a 128-bit secret as 32 lower-case hex digits, compared in
  constant time. A wrong or missing secret, or a channel that is not in
  `send_receive`, gets `404` `{"error":"not_found"}`.
- With the right secret, any method other than `POST` gets `405`.
- Every `POST` with the right secret gets `204` at once, with no body, whatever
  the delivery holds: an event other than `new-message`, a message sent from the
  Mac's own account, a GUID already handled or queued, a body over 8 KiB
  (counted in `lost`), or a full inbox (counted in `lost`; the next catch-up
  recovers it).
- New messages are queued. The receive task publishes them.
