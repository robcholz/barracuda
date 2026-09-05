# iMessage Bridge RPCs

`imessage_bridge.to_agent` resolves a complete `gateway.message.received`
document or binds a newly opened Agent session to that route. Restored mappings
explicitly report `open_required: true` because Agent session ownership is not
persisted.

`imessage_bridge.to_gateway` consumes one bounded `session.event` field chunk.
It only forwards `output_delta.text`; `turn_started` and `turn_ended` delimit a
`gateway.send_stream` stream. A forwarding decision returns a one-shot
`command_id`, and a second call exchanges that ID for the Gateway RPC request.
