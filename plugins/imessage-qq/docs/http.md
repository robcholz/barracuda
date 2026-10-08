# IMessage QQ HTTP API

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

- `204 No Content`: the credentials were verified and the provider was stored
  and registered.
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
