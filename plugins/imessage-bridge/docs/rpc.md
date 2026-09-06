# iMessage Bridge RPCs

`imessage_bridge.to_agent` resolves a complete `gateway.message.received`
document without changing reply state, or binds an open Agent session to that
route and queues the inbound `message_id`. Workflow calls the binding form for
every accepted inbound message immediately before `session.append`. Restored
mappings explicitly report `open_required: true` because Agent session
ownership is not persisted.

`imessage_bridge.to_gateway` consumes one complete bounded `session.event`
document. A user `turn_started` consumes the oldest queued inbound message and
pins it as the active `reply_to` until `turn_ended`; later inbound messages do
not alter the active turn. Non-user turns retain the mapped route with a null
`reply_to`. An unmapped session or an event outside an active turn returns an
empty object. The semantic event remains untouched for Workflow to pass to
`gateway.send_stream`. A `closed` event clears transient reply state, marks the
stored Agent session as no longer open, returns an empty object, and leaves its
route mapping available for a later explicit `session.open`.
