# Agent IMessage Gateway Plugin

`agent-imessage-gateway` depends on `agent`, `imessage-gateway`, `time`, and
`workflow`. It requires `AgentToolRegistry` and `AgentRuntime` from `agent`,
`IMessageGateway` from `imessage-gateway`, `UtcClock` from `time`, and
`WorkflowActionRegistry` from `workflow`. During Plugin registration it adds
the awaited `gateway_send` and `gateway_send_media` Tools to
`AgentToolRegistry` and registers `imessage_bridge.to_agent`,
`imessage_bridge.to_gateway` and `imessage_bridge.session` in
`WorkflowActionRegistry`.

## Sessions per conversation

A route (a channel conversation) owns the sessions it started and has at most
one current session, which receives its messages. When it has none, the next
message starts one: `imessage_bridge.to_agent` answers `{ persistence }`
(`ephemeral` after `/new temp`) and the `imessage-to-agent` Workflow creates
the session with it. A turn that started while the route was on a session
finishes there even after the route leaves it; later output of a session the
route left reaches no one. A temporary session's mapping is never saved, and
leaving it deletes the session.

`imessage_bridge.session` carries out the session controls of
`gateway.control.received` (everything but `interrupt` and `cancel`, which the
`imessage-control-to-agent` Workflow sends to `session.interrupt` and
`session.cancel`): list, new, switch, rename (`AgentRuntime::rename_session`)
and delete (asking first unless `confirm`). It lists the route's saved
sessions newest first by their last use (`AgentRuntime::describe_sessions`),
forgets sessions the Agent no longer has, and answers the conversation through
`IMessageGateway::send_sessions` with the `UtcClock` time.

The bridge uses this Plugin's scoped key-value storage for the durable Gateway
route to Agent session mapping. Pending inbound replies, the active turn
reply, and route reservations for sessions being created remain runtime-only
state. Action registration guards are retained
for the Plugin lifetime. The Action request and response contracts are in
`schemas/action`.

`imessage_bridge.to_agent` takes one of three requests: `{ route, message_id,
text }` resolves the session for an inbound message (reserving an unmapped
route while its session is created), `{ route, message_id, session }` binds
it, and `{ route }` only looks the route up, for `gateway.control.received`:
the Agent Plugin's `imessage-control-to-agent` Workflow maps a control to
`session.interrupt` or `session.cancel` on the session it finds, and does
nothing for a route with no open session.

The Plugin owns no channel, streaming runtime, or background task.
