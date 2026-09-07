# Lua VM Workflow Events

Every accepted run emits ordered events with its `run_id` and a zero-based
`sequence` shared across that run.

`vm.output` carries one complete `io.print(...)` message:

```json
{"run_id":1,"sequence":0,"chunk":"hello","message_end":true}
```

The legacy `chunk` and `message_end` fields remain compatible with existing
Workflow definitions, but output is no longer split at an Event Router lane
boundary.

`vm.input_required` is emitted when Lua blocks in `io.input()`:

```json
{"run_id":1,"sequence":1}
```

`vm.finished` reports `success`, `cancelled`, or an execution error:

```json
{"run_id":1,"sequence":2,"outcome":"error","error":"lua_runtime","diagnostic":"detail"}
```

Stable execution error codes are `vm_create`, `vm_configure`, `lua_load`,
`lua_runtime`, `unexpected_yield`, and `lua_memory`.
