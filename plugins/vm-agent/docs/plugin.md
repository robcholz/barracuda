# VM Agent Plugin

- Plugin ID: `vm-agent`
- Direct Plugin dependencies: `agent`, `vm`
- Required typed capabilities: `AgentRuntime` from `agent` and
  `LuaPackageRegistry` from `vm`
- Provided typed capabilities: none
- Owned tasks: one Agent ask adapter service

This Plugin registers the require-only `agent` Lua package for every VM
execution. It owns the VM-facing API shape and translates each request into a
fresh ephemeral Agent session. The Agent Plugin only provides its typed runtime;
it does not know about Lua, streams, or this adapter's response contract.

Lua API:

- `agent.ask(text, stream) -> output | nil, error` starts an isolated,
  non-persistent ask.
- `output:next() -> text | nil, error` returns the next value.
- `output:next() -> nil` returns clean EOF after the temporary session has been
  deleted.
- `output:close()` cancels the ask early. Lua 5.4 to-be-closed variables are also
  supported.

Both modes return the same `output` userdata. With `stream=true`, each `next()`
returns one assistant-text delta. With `stream=false`, the adapter buffers the
assistant text and returns exactly one complete value before EOF. Start and
iteration failures follow Lua's conventional `nil, error` return shape.

Every session uses `SessionPersistence::Ephemeral` and permission level `Deny`.
The adapter allows at most two concurrent asks, accepts at most 4096 input bytes,
and permits at most 32768 assistant-output bytes per ask. Bounded output queues
apply backpressure to streaming producers.

Dropping the retained VM registration revokes callbacks in existing Lua states,
rejects new asks, cancels active asks, and lets the owner task delete their
temporary sessions before it exits.
