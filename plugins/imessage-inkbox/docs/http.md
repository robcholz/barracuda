# IMessage Inkbox HTTP API

Every route is an exact path that accepts only `POST`; any other method returns
`405 Method Not Allowed` with `{"error":"method_not_allowed"}`.

## `POST /api/gateway/inkbox`

Configures or replaces the Inkbox message-channel provider. The request body is
a JSON object containing `api_key`, `identity_id`, and `api_base`. Fields with provider defaults may be omitted.

Responses:

- `204 No Content`: the provider was configured and registered.
- `400 Bad Request`: the JSON body was invalid or required fields were absent.
- `405 Method Not Allowed`: the endpoint only accepts `POST`.
- `422 Unprocessable Content`: the Gateway rejected channel registration.

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
{"error":"<kind>","message":"<text>","code":"<upstream code>"}
```

`message` and `code` are present only when known. When Inkbox returned its own
message or code, they are passed through verbatim. Inkbox reports them as
`{"detail":"<message>"}`, `{"detail":{"code","message"}}`, or a validation
list `{"detail":[{"msg",…}]}`, whose first `msg` becomes `message`.

| Status | `error` | Meaning |
| --- | --- | --- |
| 400 | `invalid_request` | The body was invalid, or Inkbox rejected the signup request (any upstream 4xx, including its 422 validation error). |
| 405 | `method_not_allowed` | The route only accepts `POST`. |
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
`{"api_key","identity_id","api_base":"https://inkbox.ai"}` under the same key
and format as `POST /api/gateway/inkbox`, then (re)registers the channel. An
unclaimed agent can already send within its limits, so no verification is
needed before registration.

Responses:

- `200 OK`: `{"email_address":"<agent mailbox>","claim_status":"<status>"}`,
  for example `agent_unclaimed`.
- `400`, `405`, `409`, `422 registration_failed`, `500`, `502` as above.

Inkbox shows the API key only once. If the identity lookup, storage, or
registration fails after a successful upstream signup, nothing is stored and
the person signs up again.

### `POST /api/gateway/inkbox/verify`

Request body: `{"code":"123456"}`. The code must be exactly 6 ASCII digits
after trimming; anything else returns `400` without calling Inkbox. The device
submits it with the stored API key and stored `api_base`.

Responses:

- `200 OK`: `{"claim_status":"<status>"}`, for example `agent_claimed`.
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
