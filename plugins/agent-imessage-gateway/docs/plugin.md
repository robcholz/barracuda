# Agent IMessage Gateway Plugin

Exposes iMessage Agent Tools, the Workflow Actions that bridge Gateway
conversations to Agent sessions, and session commands.

- Plugin ID: `agent-imessage-gateway`
- Dependencies: `agent`, `imessage-gateway`, `time`, `workflow`
- Settings: none
- Context resources: none
- Provided capabilities: none
- Required capabilities: `AgentToolRegistry` from `agent`, `AgentRuntime` from
  `agent`, `IMessageGateway` from `imessage-gateway`, `UtcClock` from `time`,
  `WorkflowActionRegistry` from `workflow`, `WorkflowService` from `workflow`
- Workflow: Actions `imessage_bridge.to_agent`, `imessage_bridge.to_gateway`,
  `imessage_bridge.session`, see `action.md`
- Agent Tools: `gateway_send`, `send_attachment`
- Lua packages: none
- Web: none
- Tasks: one Workflow-loading task (pool size 1), spawned from `start`, which
  ends once the bundled Workflows are loaded
- Storage: Plugin-scoped KV entries keyed by Agent session ID, each holding
  the saved Gateway route of that session; reads `/resources/workflows.json`

During registration the Plugin adds the awaited `gateway_send` Tool and the
background `send_attachment` Tool to `AgentToolRegistry` as the hidden
`gateway` group, and registers the three Actions in `WorkflowActionRegistry`,
retaining their registration guards for the Plugin lifetime.

## Sending files

`send_attachment { path, caption?, kind? }` sends a file to the conversation
of the session that calls it, read from `ToolContext::session`: the route the
bridge bound that session to, replying to the message its running turn
answers. A session its route left reaches no one, as its output does, and a
session bound to no route is refused before acceptance with
`no_conversation`. The Tool sends one `GatewaySendMediaRequest::File`, so the
Gateway opens the file by path and the bytes never pass through the model or
Workflow; there is no path restriction beyond what the Gateway can open.
`kind` defaults from the MIME type the path's extension names: `image`,
`audio`, `video`, else `file`. The call is a background Tool: it is accepted
at once and completes with `{"message_id"}` or the Gateway's stable business
error `{"error"}` (`not_found`, `too_large`, `unknown_channel`,
`unsupported`, `authentication`, `rate_limited`, `delivery`,
`invalid_request`).

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
text, attachments? }` resolves the session for an inbound message (reserving
an unmapped route while its session is created), `{ route, message_id, session }` binds
it, and `{ route }` only looks the route up, for `gateway.control.received`:
the `imessage-control-to-agent` Workflow maps a control to
`session.interrupt` or `session.cancel` on the session it finds, and does
nothing for a route with no open session.

## Bundled Workflows

The Plugin ships the Workflows that connect the Gateway and the Agent in its
scoped `/resources/workflows.json`: `imessage-to-agent` and
`imessage-control-to-agent` route `gateway.message.received` and
`gateway.control.received` to Agent sessions, and `agent-to-imessage` sends
each `session.event` back to its route with `gateway.send_stream`.
`imessage-to-agent` passes a message's `attachments` (references to the files
the Gateway stored, never their bytes) to `session.append` or
`session.respond`. A Workflow link fails on a missing field, and the Gateway
leaves `attachments` out of a message without files, so the Workflow branches
on `$event.input.attachments` being `null` and sends the field only when it is
there. At start,
once every Plugin has registered its Actions, a Plugin-owned task loads each
definition transiently through `WorkflowService` and ends. The definitions stay
in `/resources` and are never copied into the user-created
`/data/workflows.json` catalog; a persisted user definition with the same ID
takes precedence. A missing or invalid resource, or a rejected definition, is
logged and the remaining definitions are not loaded.

The Plugin owns no channel or streaming runtime.
