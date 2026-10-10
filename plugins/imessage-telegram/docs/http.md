# IMessage Telegram HTTP API

The shared shapes come from `barracuda-imessage-gateway-channel`; see the
Gateway's [`plugin.md`](../../imessage-gateway/docs/plugin.md).

## `GET /api/gateway/telegram/status`

Returns `200` with the channel status. It never returns settings or the token;
other methods answer `405`.

```json
{"configured":true,"mode":"send_receive",
 "receive":{"state":"receiving","slots":{"in_use":1,"capacity":2}},
 "owners":{"count":1}}
```

- `mode` is `disabled`, `send`, or `send_receive`. An unconfigured channel
  reports the mode it will start in (`send_receive`).
- `receive` is present only in `send_receive`. `state` is `idle`, `starting`,
  `receiving`, `no_slot`, or `error`; `message` (Telegram's description, or a
  connection failure) comes only with `error`, and `capacity` only with
  `no_slot`. `slots` is the device's receive slots in use across every channel
  and how many it has.
- `owners.count` is the number of allowed accounts.

## `POST /api/gateway/telegram`

Configures or replaces the Telegram message-channel provider. The request body is
a JSON object containing `token`, `api_base`, and `draft_min_delta_bytes`. Fields
with provider defaults may be omitted. A new configuration restarts receiving;
a different bot token also clears the receive cursor.

Responses:

- `204 No Content`: the provider was configured, and registered with the
  Gateway unless the mode is `disabled`.
- `400 Bad Request`, `{"error":"invalid_request"}`: the JSON body was invalid
  or required fields were absent.
- `405 Method Not Allowed`: the endpoint only accepts `POST`.
- `422 Unprocessable Content`, `{"error":"registration_failed"}`: the Gateway
  rejected channel registration. The previous stored configuration is restored
  and no Telegram channel stays registered.
- `500 Internal Server Error`, `{"error":"storage"}`: storage failed.

Credentials are accepted only in the request body and are never included in the
response or logs.

## `POST /api/gateway/telegram/mode`

Body `{"mode":"disabled"|"send"|"send_receive"}`.

- `204`: the mode is stored and applied: the `telegram` Gateway channel is
  registered unless `disabled`, and the receive loop runs only in
  `send_receive`.
- `409`, `{"error":"no_slot","capacity":n}`: `send_receive` was stored, but
  every receive slot is in use; receiving reports `no_slot` and starts once a
  slot frees.
- `400` `invalid_request` or `unsupported_mode`; `422` `registration_failed`
  (the previous mode stays); `500` `storage`; `405` for other methods.

## `GET /api/gateway/telegram/owners`

Returns the allowed accounts and the binding code, minting a code when none is
valid:

```json
{"owners":[{"id":"42","label":"@ann"}],
 "pairing":{"code":"012345","expires_in":600},"ignored":3}
```

`id` is the Telegram user id, `label` the `@username` or first name. `pairing`
is `null` while eight accounts are allowed or the Platform has no entropy.
`ignored` counts messages dropped since boot because their sender is not
allowed. A Telegram user binds by sending the code, or `/start <code>`, to the
bot; the device answers once and the code rotates. Before the channel is
configured, `GET` and `POST` answer `409` `{"error":"not_configured"}`: it holds
no owner book yet.

## `POST /api/gateway/telegram/owners`

`{"remove":"<id>"}` removes an account (an unknown id is fine) and
`{"rotate":true}` mints a new code; both answer `204`. `400`
`invalid_request` for any other body, `500` `storage`, `503`
`entropy_unavailable`.
