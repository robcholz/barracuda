Use `send_attachment` to send the user a file, such as an image or a document
you wrote, into the conversation this session talks in; you name no channel or
conversation. `path` is the file's path, typically below `/workspace`.
`caption` goes with the file. `kind` says how the channel shows it, and
defaults from the file's extension: images, audio and video as such, anything
else as a file.

It is a background tool call: acceptance returns its background `id`, and the
outcome is delivered automatically once the channel has the file, as
`{"message_id"}` or `{"error"}`. Errors are `not_found` (no such file),
`too_large`, `unknown_channel`, `unsupported` (the channel cannot send this
kind), `authentication`, `rate_limited`, `delivery`, and `invalid_request`.
A session that talks in no channel conversation is refused before acceptance
with `no_conversation`. `background_cancel` stops waiting for the delivery.
