# Lua VM JSON RPCs

All VM RPCs are unary JSON contracts with visibility `"*"`. A successful
`vm.run` starts an execution and releases its RPC lane immediately. Execution
output and completion use the Events in [event.md](event.md); they are not
collected into the RPC response.

## `vm.run`

- Request schema: `schemas/rpc/run/request.json`
- Response schema: `schemas/rpc/run/response.json`
- Maximum encoded request: 512 bytes
- Maximum encoded response: 48 bytes

Request:

```json
{"source":"local io=require('io'); io.print('hello')"}
```

Accepted response:

```json
{"run_id":1}
```

The `run_id` is a nonzero `u32`, is unique among the four active executions,
and correlates all later control calls and Events. The source is copied once
from the lane into a fixed 480-byte task argument because execution outlives
the request lane; this does not allocate a per-run source `String`. JSON
escaping and the object envelope must also fit the 512-byte encoded request
limit.

Stable rejections are `source_limit_exceeded`, `runtime_unavailable`, and
`busy`. A `busy` response means all four task/memory slots are occupied.

## `vm.input`

- Request schema: `schemas/rpc/input/request.json`
- Response schema: `schemas/rpc/input/response.json`
- Maximum encoded request: 512 bytes
- Maximum encoded response: 48 bytes

Supply one complete input value after `vm.input_required`:

```json
{"run_id":1,"input":"barracuda"}
```

Close the input side so later `io.input()` calls return `nil`:

```json
{"run_id":1,"eof":true}
```

Success is `{}`. The input queue holds one complete message and therefore
preserves backpressure without accumulating caller input. The default logical
input limit is 400 UTF-8 bytes. Stable rejections are `run_not_found`,
`input_limit_exceeded`, `input_backpressure`, and `input_closed`.

## `vm.cancel`

- Request schema: `schemas/rpc/cancel/request.json`
- Response schema: `schemas/rpc/cancel/response.json`
- Maximum encoded request: 32 bytes
- Maximum encoded response: 48 bytes

Request:

```json
{"run_id":1}
```

Success is `{}`; an unknown or already released execution returns
`{"error":"run_not_found"}`. Cancellation closes pending input and is observed
at the next async suspension or Lua instruction-hook boundary. The terminal
Event reports `"outcome":"cancelled"`.

Malformed JSON and request shapes are Event Router `RpcError`s. The stable
rejections above are business response documents. Lua load/runtime/memory
failures occur after acceptance and therefore appear only in `vm.finished`.
