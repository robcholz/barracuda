# IMessage Telegram HTTP API

## `POST /api/gateway/telegram`

Configures or replaces the Telegram message-channel provider. The request body is
a JSON object containing `token`, `api_base`, and `draft_min_delta_bytes`. Fields with provider defaults may be omitted.

Responses:

- `204 No Content`: the provider was configured and registered.
- `400 Bad Request`: the JSON body was invalid or required fields were absent.
- `405 Method Not Allowed`: the endpoint only accepts `POST`.
- `422 Unprocessable Content`: the Gateway rejected channel registration.

Credentials are accepted only in the request body and are never included in the
response or logs.
