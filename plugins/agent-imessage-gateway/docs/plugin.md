# Agent IMessage Gateway Plugin

`agent-imessage-gateway` depends on `agent`, `imessage-gateway`, and `workflow`.
During Plugin registration it adds the awaited `gateway_send` and
`gateway_send_media` Tools to `AgentToolRegistry` and registers
`imessage_bridge.to_agent` plus `imessage_bridge.to_gateway` in
`WorkflowActionRegistry`.

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
