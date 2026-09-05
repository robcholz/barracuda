# Lua VM Events

VM execution is an application-level stream of bounded JSON Events. Every
Event carries the `run_id` returned by `vm.run` and a zero-based `sequence`
shared across all Events for that run. Every encoded Event input is at most
416 bytes, leaving room for Event Router's ingress envelope in a 512-byte lane.

## `vm.output`

Emitted for `io.print(...)` output:

```json
{"run_id":1,"sequence":1,"chunk":"hello","message_end":true}
```

`chunk` contains at most 48 UTF-8 bytes. Long print messages produce multiple
Events split only at UTF-8 boundaries. `message_end` is true only on the last
chunk of that logical print call; an empty print emits one empty terminal
chunk. Each Event is submitted before execution advances, so Event Router
capacity applies backpressure instead of allowing an unbounded output buffer.

## `vm.input_required`

Emitted when Lua reaches `io.input()`:

```json
{"run_id":1,"sequence":0}
```

The execution waits until the caller uses `vm.input` to supply one value or
EOF, or uses `vm.cancel`. One already-queued input is allowed, but the Event is
still emitted when Lua actually requests it.

## `vm.finished`

Exactly one terminal outcome is attempted after an accepted run.

Successful completion:

```json
{"run_id":1,"sequence":2,"outcome":"success"}
```

Cancellation:

```json
{"run_id":1,"sequence":2,"outcome":"cancelled"}
```

Execution failure:

```json
{"run_id":1,"sequence":2,"outcome":"error","error":"lua_runtime","diagnostic":"bounded detail"}
```

Stable execution error codes are `vm_create`, `vm_configure`, `lua_load`,
`lua_runtime`, `unexpected_yield`, and `lua_memory`. Diagnostics are limited to
48 UTF-8 bytes and are informational; callers branch on `error`.

If Event Router cannot accept an Event because its transport is shutting down
or invalid, the VM does not accumulate or retry an unbounded copy. In that
case delivery of a later terminal Event cannot be guaranteed through the same
failed transport.
