Use `gateway_send_media` with `start`, one or more ordered base64 `chunk`
commands, then `finish`. Keep the same `stream_id` and increase `sequence` for
every command after `start`.
