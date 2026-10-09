# IMessage WeChat HTTP API

## `GET /api/gateway/wechat/status`

Returns `200` with the shared channel status (`status_response`); other methods
answer `405`:

```json
{"configured":true,"mode":"send_receive",
 "receive":{"state":"receiving","slots":{"in_use":1,"capacity":2}},
 "owners":{"count":1}}
```

- `configured`: a WeChat bot is linked (a configuration is stored).
- `mode`: `disabled` or `send_receive`.
- `receive` (only in `send_receive`): `state` is `idle`, `starting`,
  `receiving`, `no_slot` (with `capacity`), or `error` (with `message`).
  After iLink reports the bot session expired (code -14) it is
  `{"state":"error","message":"需要重新扫码 / Scan again to relink"}` until a
  new QR login is confirmed.
- `owners.count`: allowed accounts.

It never returns settings or the token; `POST /api/gateway/wechat` takes
changes only.

## `POST /api/gateway/wechat/mode`

Body `{"mode":"disabled"}` or `{"mode":"send_receive"}`; `send` is not offered
because iLink sends need inbound activity.

- `204`: applied. `disabled` unregisters the channel and stops receiving;
  `send_receive` registers it (when linked) and starts receiving.
- `409` `{"error":"no_slot","capacity":n}`: every receive slot is in use. The
  mode is saved; receiving starts once a slot frees.
- `400` `{"error":"invalid_request"}` or `{"error":"unsupported_mode"}`,
  `422` `{"error":"registration_failed"}`, `500` `{"error":"storage"}`,
  `405` for other methods.

## `GET`/`POST /api/gateway/wechat/owners`

The allowed accounts (授权账号) and the pairing code (绑定码), served by the
shared `OwnersEndpoint`. `GET` answers
`{"owners":[{"id":"…@im.wechat","label":null}],"pairing":{"code":"012345","expires_in":600},"ignored":0}`;
`POST` `{"remove":"<id>"}` or `{"rotate":true}` answers `204`. A WeChat owner
id is the sender's `from_user_id`; it has no label. A message whose trimmed
text is the current code adds its sender and is answered once with
「已绑定，可以开始对话了」 / "Paired. You can start chatting.". Before a bot is
linked both answer `409` `{"error":"not_configured"}`: there is no owner book
yet.

## `POST /api/gateway/wechat`

Configures or replaces the WeChat message-channel provider. The request body is
a JSON object containing `token`, `api_base`, `app_id`, `client_version`, `route_tag`, and `x_wechat_uin`. Fields with provider defaults may be omitted.

Responses:

- `204 No Content`: the provider was configured and, unless the mode is
  `disabled`, registered. Receiving restarts with the new configuration.
- `400 Bad Request` `{"error":"invalid_request"}`: the JSON body was invalid or
  required fields were absent.
- `405 Method Not Allowed` `{"error":"method_not_allowed"}`: the endpoint only
  accepts `POST`.
- `422 Unprocessable Content` `{"error":"registration_failed"}`: the Gateway
  rejected channel registration. The previous stored configuration is restored.
- `500 Internal Server Error` `{"error":"storage"}`: the configuration could not
  be read, written, or rolled back.

Credentials are accepted only in the request body and are never included in the
response or logs.

## `/api/gateway/wechat/login`

Runs a WeChat iLink QR login so the bot token never has to be typed. The device
makes every iLink call, because iLink sends no CORS headers for the portal.

At most one login session exists. A single task owned by this Plugin performs
the iLink requests; `get_qrcode_status` is a long poll that iLink holds for
about 31 s, so no browser request ever waits on it. The browser polls `GET`,
which answers immediately from the task's last observed state.

The path accepts `POST`, `GET`, and `DELETE`. Any other method returns
`405 Method Not Allowed` `{"error":"method_not_allowed"}`.

### `POST`: start a session

The body is empty or `{}`; any other body returns `400 Bad Request`
`{"error":"invalid_request"}` and changes nothing.

The device cancels any running session, then requests a QR code with
`GET {api_base}/ilink/bot/get_bot_qrcode?bot_type=3`. `api_base` is the stored
configuration's `api_base`, or `https://ilinkai.weixin.qq.com` when nothing is
stored. On success the session starts and the response is:

- `200 OK` `{"url":"<qrcode_img_content>","expires_in":480}`: `url` is the
  page URL to render as a QR code for the user to scan with WeChat.
  `expires_in` is the session lifetime in seconds.
- `502 Bad Gateway`
  `{"error":"upstream_unavailable","message":"<text>","code":"<iLink code>"}`:
  iLink was unreachable (transport, DNS, or TLS failure), answered with
  something other than a JSON object or with a non-2xx status, omitted the QR
  fields, or reported an error. When iLink reported its own error (`ret` or
  `errcode` other than 0), `code` is that value and `message` is its
  `errmsg`/`message`, passed through verbatim; otherwise `code` is absent. No
  session runs afterwards and `GET` reports `idle`.

Concurrent `POST` and `DELETE` requests are serialized.

### `GET`: observe the session

Always returns `200 OK` immediately:

```json
{"status":"wait","configured":false}
```

- `status` is one of:
  - `idle`: no session has run since boot, or the last one was cancelled;
  - `wait`: the QR code is issued and not scanned yet;
  - `scanned`: the QR code was scanned and awaits confirmation on the phone
    (iLink's `scaned`);
  - `confirmed`: the user confirmed; the channel is stored and registered;
  - `expired`: the QR code expired, or the session reached 480 s;
  - `failed`: a status poll failed or iLink reported an error, or storing or
    registering the confirmed channel failed. A `message` field describes the
    failure.
- `configured` says whether a WeChat bot is linked (a configuration is
  stored), from either this login or `POST /api/gateway/wechat`, whatever the
  session state or mode.

The final state of a finished session stays readable until the next `POST` or
`DELETE`.

### `DELETE`: cancel the session

Cancels the running session, if any, closes its iLink request, and returns
`204 No Content`. `GET` then reports `idle`. The page calls it when it closes.

### Session lifecycle

After the QR code is issued, the task long-polls
`GET {api_base}/ilink/bot/get_qrcode_status?qrcode=<id>`, waiting at least 1 s
between polls. iLink statuses are `wait`, `scaned`, `confirmed`, and `expired`;
any other status ends the session as `failed`.

On `confirmed` iLink returns `bot_token`, `ilink_bot_id`, and `baseurl`. The
task stores `{"token":"<bot_token>","api_base":"<baseurl, or the api_base used
for the login>"}` with every other field at its default, through the same path
as `POST /api/gateway/wechat`: the configuration is persisted, the channel is
re-registered, and a Gateway rejection restores the previous configuration.
When `ilink_bot_id` differs from the bot of the previous login, the stored
`getupdates` cursor and context tokens are cleared. Receiving then restarts,
which also clears an expired-session error. It then reports `confirmed`, or
`failed` if storing or registration failed.

The session ends on `confirmed`, `expired`, after 480 s, on `DELETE`, on a new
`POST`, or when the Plugin stops. While no session runs the task is parked and
holds no iLink connection.

The bot token is never included in a response or log.
