# IMessage QQ HTTP API

## `GET /api/gateway/qq/status`

Returns `200` with the shared channel status (see the
[Gateway's channel HTTP surface](../../imessage-gateway/docs/plugin.md)):

```json
{"configured":true,"mode":"send_receive",
 "receive":{"state":"receiving","slots":{"in_use":1,"capacity":1}},
 "owners":{"count":1}}
```

`configured` is whether a configuration is stored. `receive` is present only
in `send_receive`; its `state` is `idle`, `starting`, `receiving`, `no_slot`
(with `capacity`), or `error` (with `message`, for example
「QQ 机器人已下架，只能连接沙箱环境 / The QQ bot is delisted and may only use the
sandbox」 after close code 4914). It never returns settings, the App Secret,
or a token, and makes no request to QQ. Other methods answer `405`.

## `POST /api/gateway/qq/mode`

`{"mode":"disabled"|"send"|"send_receive"}`. 204 when applied; 409
`{"error":"no_slot","capacity":n}` when `send_receive` finds every receive
slot in use (the mode is saved and receiving starts once a slot frees); 400
`invalid_request` or `unsupported_mode`; 422 `registration_failed`; 500
`storage`.

## `GET` and `POST /api/gateway/qq/owners`

`GET` answers
`{"owners":[{"id":"<openid>","label":null}],"pairing":{"code":"012345","expires_in":600},"ignored":0}`.
Owner ids are QQ openids: `user_openid` for direct messages and
`member_openid` in groups. `POST` takes `{"remove":"<id>"}` or
`{"rotate":true}` and answers 204. Before the channel is configured both answer
`409` `{"error":"not_configured"}`: there is no owner book yet.

## `POST /api/gateway/qq`

Configures or replaces the QQ message-channel provider. The request body is a
JSON object:

| Field | Required | Default |
| --- | --- | --- |
| `app_id` | yes | |
| `app_secret` | yes | |
| `api_base` | no | `https://api.sgroup.qq.com` |
| `token_url` | no | `https://bots.qq.com/app/getAppAccessToken` |

Unknown fields are rejected, including the former `access_token`.

Before storing anything, the device exchanges the App Secret for one access
token: `POST {token_url}` with `{"appId":"<app_id>","clientSecret":"<app_secret>"}`.
QQ answers `{"access_token":"...","expires_in":"7200"}` (`expires_in` may be a
string or a number). A reply without `access_token`, such as HTTP 200
`{"code":10004,"message":"机器人不存在"}`, is a rejection. Only after a token is
issued is the configuration stored and the channel registered with the Gateway.
The token itself is never stored.

Responses:

- `204 No Content`: the credentials were verified and the provider was stored;
  it is registered with the Gateway unless the mode is `disabled`, and
  receiving restarts with it. Configuring a different App ID or API origin
  forgets the stored gateway session.
- `400 Bad Request` `{"error":"invalid_request"}`: the JSON body was invalid,
  required fields were absent, or an unknown field was present. Nothing is
  requested from QQ.
- `405 Method Not Allowed` `{"error":"method_not_allowed"}`: the endpoint only
  accepts `POST`.
- `422 Unprocessable Content`
  `{"error":"verification_failed","message":"<QQ message>","code":"<QQ code>"}`:
  QQ refused to issue a token. `message` and `code` are QQ's own, passed through
  verbatim, and each is present only when QQ sent it. Nothing is stored and the
  current channel is unchanged.
- `422 Unprocessable Content` `{"error":"registration_failed"}`: the Gateway
  rejected channel registration. The previous stored configuration is restored.
- `502 Bad Gateway` `{"error":"upstream_unavailable","message":"<text>"}`: the
  token endpoint was unreachable (transport, DNS, or TLS failure), answered with
  something other than JSON, answered with a 5xx status, or issued a token
  without a valid `expires_in`. Nothing is stored.
- `500 Internal Server Error` `{"error":"storage"}`: the configuration could not
  be read, written, or rolled back.

The App Secret and access tokens are accepted only in request bodies and are
never included in a response or log.

QQ destinations use a typed Gateway conversation ID: `c2c:<openid>` for direct
messages, `group:<group_openid>` for group messages, or `channel:<channel_id>`
for guild-channel messages.
