# IMessage BlueBubble HTTP API

## `POST /api/gateway/bluebubbles`

Configures or replaces the BlueBubble message-channel provider. The request body is
a JSON object containing `server_url`, `password`, `use_private_api`, `stream_edit_min_delta_bytes`, and `stream_max_edits`. Fields with provider defaults may be omitted.

Responses:

- `204 No Content`: the provider was configured and registered.
- `400 Bad Request`: the JSON body was invalid or required fields were absent.
- `405 Method Not Allowed`: the endpoint only accepts `POST`.
- `422 Unprocessable Content`: the Gateway rejected channel registration.

Credentials are accepted only in the request body and are never included in the
response or logs.
