# IMessage Telegram HTTP API

## `GET /api/gateway/telegram`

Returns `200` with `{"configured": true}` while a Telegram channel is
registered with the Gateway, otherwise `{"configured": false}`. It never
returns settings or the token.

## `POST /api/gateway/telegram`

Configures or replaces the Telegram message-channel provider. The request body is
a JSON object containing `token`, `api_base`, and `draft_min_delta_bytes`. Fields with provider defaults may be omitted.

Responses:

- `204 No Content`: the provider was configured and registered.
- `400 Bad Request`: the JSON body was invalid or required fields were absent.
- `405 Method Not Allowed`: the endpoint only accepts `GET` and `POST`.
- `422 Unprocessable Content`, `{"error":"registration_failed"}`: the Gateway
  rejected channel registration. The previous stored configuration is restored
  and no Telegram channel stays registered.
- `500 Internal Server Error`, `{"error":"storage"}`: storage failed.

Credentials are accepted only in the request body and are never included in the
response or logs.
