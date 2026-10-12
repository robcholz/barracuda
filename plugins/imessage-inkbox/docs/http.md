# IMessage Inkbox HTTP API

Every route accepts `POST` except `/api/gateway/inkbox/status`, which accepts
only `GET`; `/api/gateway/inkbox/owners` accepts both. Any other method returns
`405 Method Not Allowed` with `{"error":"method_not_allowed"}`.

## `GET /api/gateway/inkbox/status`

Returns `200` with the shared channel status (see the
[Gateway's channel HTTP surface](../../imessage-gateway/docs/plugin.md)) plus,
when the stored configuration came from signup,
`"signup": {"human_email": "<address entered>", "email_address": "<agent mailbox>", "claim_status": "<status>"}`:

```json
{"configured":true,"mode":"send_receive",
 "receive":{"state":"receiving","slots":{"in_use":1,"capacity":1}},
 "owners":{"count":1},
 "signup":{"human_email":"person@example.com","email_address":"barracuda-a1b2@inkboxmail.com","claim_status":"agent_claimed"}}
```

`configured` is whether a configuration is stored. `receive` is present only
in `send_receive`; its `state` is `idle`, `starting`, `receiving`, `no_slot`
(with `capacity`), or `error` (with `message`). `human_email` is the address
the person entered (trimmed), which Inkbox sent the code to. `claim_status` is
the last one Inkbox reported, at signup or at a later verify. The API key and
identity are never returned.

## `POST /api/gateway/inkbox/mode`

`{"mode":"disabled"|"send"|"send_receive"}`. 204 when applied; 409
`{"error":"no_slot","capacity":n}` when `send_receive` finds every receive
slot in use (the mode is saved and receiving starts once a slot frees); 400
`invalid_request` or `unsupported_mode`; 422 `registration_failed`; 500
`storage`.

## `GET` and `POST /api/gateway/inkbox/owners`

`GET` answers
`{"owners":[{"id":"+15555550123","label":null}],"pairing":{"code":"012345","expires_in":600},"ignored":0}`;
owner ids are Inkbox `remote_number`s. `POST` takes `{"remove":"<id>"}` or
`{"rotate":true}` and answers 204. Before the channel is configured both answer
`409` `{"error":"not_configured"}`: there is no owner book yet.

## `POST /api/gateway/inkbox`

Configures or replaces the Inkbox message-channel provider. The request body is
a JSON object containing `api_key`, `identity_id`, and `api_base`. Fields with provider defaults may be omitted.

Responses:

- `204 No Content`: the provider was configured; it is registered with the
  Gateway unless the mode is `disabled`, and receiving restarts with it. A
  different API key, identity, or origin starts from that identity's newest
  message.
- `400 Bad Request`: the JSON body was invalid or required fields were absent.
- `405 Method Not Allowed`: the endpoint only accepts `POST`.
- `422 Unprocessable Content` `{"error":"registration_failed"}`: the Gateway
  rejected channel registration. The previous stored configuration is restored
  and no Inkbox channel stays registered.
- `500 Internal Server Error` `{"error":"storage"}`: storage failed.

The stored configuration has no `signup` record afterwards.

Credentials are accepted only in the request body and are never included in the
response or logs.

## Agent self-signup

The routes below let the device create its own Inkbox agent from a person's
email address. The device makes every upstream call, because Inkbox sends no
CORS headers for the portal's origin. The Inkbox API key never leaves the
device: it is written straight into the stored configuration and is never
returned or logged.

Upstream calls use the production origin `https://inkbox.ai` and follow the
Inkbox SDK (`inkbox` on PyPI, `sdk/python/inkbox/client.py` and
`sdk/python/inkbox/identities/resources/identities.py` in
<https://github.com/inkbox-ai/inkbox>):

| Step | Upstream request |
| --- | --- |
| Signup | `POST /api/v1/agent-signup` without authentication |
| Identity | `GET /api/v1/identities/{agent_handle}` with `X-API-Key` |
| Verify | `POST /api/v1/agent-signup/verify` with `X-API-Key` |
| Resend | `POST /api/v1/agent-signup/resend-verification` with `X-API-Key` |

Only one signup, verify, or resend flow runs at a time, so the Plugin holds at
most one upstream connection for them. A request that arrives while another
flow runs returns `409` `conflict` immediately instead of waiting.

### Error body

Every error response of these routes is JSON:

```json
{"error":"<kind>","message":"<text>","code":"<upstream code>","retry":true}
```

`message` and `code` are present only when known. `retry` is present, and
`true`, only on a signup failure after Inkbox accepted the signup: repeating
the same request resumes it (see below). When Inkbox returned its own
message or code, they are passed through verbatim. Inkbox reports them as
`{"detail":"<message>"}`, `{"detail":{"code","message"}}`, or a validation
list `{"detail":[{"msg",…}]}`, whose first `msg` becomes `message`.

| Status | `error` | Meaning |
| --- | --- | --- |
| 400 | `invalid_request` | The body was invalid, or Inkbox rejected the signup request (any upstream 4xx, including its 422 validation error). |
| 405 | `method_not_allowed` | The method is not accepted by the route. |
| 409 | `conflict` | No configuration is stored for verify or resend, or another flow is running. |
| 422 | `verification_failed` | Inkbox rejected the code or the stored API key (any upstream 4xx on verify or resend). |
| 422 | `registration_failed` | The Gateway rejected the channel after signup. |
| 500 | `storage` | Plugin storage could not be read or written. |
| 502 | `upstream_unavailable` | Transport, DNS, or TLS failure; a 5xx or non-JSON reply; an incomplete success reply; or a failed identity lookup after signup. |

### `POST /api/gateway/inkbox/signup`

Request body: `{"email":"person@example.com"}`. Surrounding whitespace is
trimmed; an empty address returns `400` without calling Inkbox.

The device calls Inkbox signup with `human_email` set to that address,
`display_name` `Barracuda`, `harness` `barracuda`, and `note_to_human`
"Barracuda, your device, set up an Inkbox mailbox to send and receive mail for
you. Enter the code from this email on the device portal." Inkbox then emails
a 6-digit code to the person.

The returned `agent_handle` is resolved to the identity UUID with the identity
lookup above (the response's `id`), and the device stores
`{"api_key","identity_id","api_base":"https://inkbox.ai","signup":{"human_email","email_address","claim_status"}}`
under the same key as `POST /api/gateway/inkbox`, then (re)registers the
channel. An
unclaimed agent can already send within its limits, so no verification is
needed before registration.

Responses:

- `200 OK`: `{"email_address":"<agent mailbox>","claim_status":"<status>"}`,
  for example `agent_unclaimed`.
- `400`, `405`, `409`, `422 registration_failed`, `500`, `502` as above.

Inkbox shows the API key only once. If the identity lookup (`502`), storage
(`500`), or registration (`422 registration_failed`) fails after a successful
upstream signup, nothing is stored, the error carries `"retry": true`, and the
device keeps the signup result in RAM until it is stored or the Plugin
unloads. A later signup for the same email (after trimming) resumes from it:
Inkbox signup is not called again, so no second email is sent, and an identity
already resolved is not looked up again. A signup for a different email drops
the kept result and signs up anew.

### `POST /api/gateway/inkbox/verify`

Request body: `{"code":"123456"}`. The code must be exactly 6 ASCII digits
after trimming; anything else returns `400` without calling Inkbox. The device
submits it with the stored API key and stored `api_base`.

Responses:

- `200 OK`: `{"claim_status":"<status>"}`, for example `agent_claimed`. When
  the stored configuration came from signup with this key, its `claim_status`
  is updated; failing to store it is logged and does not change the response.
- `409 conflict`: no configuration is stored.
- `422 verification_failed`: Inkbox rejected the code, with its message (for
  example `Invalid verification code`).
- `400`, `405`, `500`, `502` as above.

### `POST /api/gateway/inkbox/resend`

No request body is required; any body is ignored. The device asks Inkbox to
send the verification email again with the stored API key. Inkbox applies a
5-minute cooldown.

Responses:

- `204 No Content`: Inkbox accepted the resend.
- `409 conflict`: no configuration is stored.
- `422 verification_failed`: Inkbox rejected the request, for example during
  the cooldown, with its message.
- `405`, `500`, `502` as above.

Verify and resend use whatever configuration is stored, including one saved
through `POST /api/gateway/inkbox`; Inkbox decides whether that key belongs to
an agent awaiting verification.
