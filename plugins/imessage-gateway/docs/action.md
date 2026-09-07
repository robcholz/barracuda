# IMessage Gateway Workflow Actions

The Plugin registers three typed Actions with static schemas under
`schemas/action/<address>/`:

- `gateway.send` sends one complete text message and returns `message_id`.
- `gateway.send_stream` accepts one ordered semantic Agent event.
- `gateway.send_media` accepts `start`, `chunk`, and `finish` commands for one
  binary stream.

Business failures are ordinary `{"error":"<code>"}` Action responses. The
four active-stream limit and per-stream queue backpressure are runtime limits.
