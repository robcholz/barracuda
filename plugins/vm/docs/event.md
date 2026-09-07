# Lua VM Workflow Events

Every accepted run emits ordered events with its `run_id` and a zero-based
`sequence` shared across that run.

`vm.output` carries one complete `io.print(...)` message:

```json
{"run_id":1,"sequence":0,"chunk":"hello","message_end":true}
```

`chunk` contains the complete message and `message_end` is always `true`.

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
