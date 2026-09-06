# iMessage Bridge RPCs

`imessage_bridge.to_agent` resolves a complete `gateway.message.received`
document or binds a newly opened Agent session to that route. Restored mappings
explicitly report `open_required: true` because Agent session ownership is not
persisted.

`imessage_bridge.to_gateway` consumes one complete bounded `session.event`
document and uses only its `session` and `type`. A mapped session returns its
stored Gateway `route` and current `reply_to`; an unmapped session returns
an empty object. The semantic event remains untouched for Workflow to pass
to `gateway.send_stream`. A `closed` event marks the stored Agent session as no
longer open, returns an empty object, and leaves its route mapping available
for a later explicit `session.open`.
