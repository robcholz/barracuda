# IMessage QQ HTTP API

## `POST /api/gateway/qq`

Configures or replaces the QQ message-channel provider. The request body is a
JSON object containing required `app_id` and `access_token` strings. `api_base`
is optional and defaults to `https://api.sgroup.qq.com`.

Responses:

- `204 No Content`: the provider was configured and registered.
- `400 Bad Request`: the JSON body was invalid or required fields were absent.
- `405 Method Not Allowed`: the endpoint only accepts `POST`.
- `422 Unprocessable Content`: the Gateway rejected channel registration.

Credentials are accepted only in the request body and are never included in the
response or logs.

QQ destinations use a typed Gateway conversation ID: `c2c:<openid>` for direct
messages, `group:<group_openid>` for group messages, or `channel:<channel_id>`
for guild-channel messages.
