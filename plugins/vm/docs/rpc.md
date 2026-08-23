# Lua VM RPC

The VM Component exposes one typed, bidirectional streaming RPC. One call owns
one isolated Lua state from source upload through completion.

| Address | Request | Response | Method error | Shape |
| --- | --- | --- | --- | --- |
| `vm.run` | `RunRequestFrame` | `RunResponseFrame` | `RunError` | streaming → streaming |

The method is intentionally typed-only. It does not opt into Event Router's
runtime JSON surface, because a long-lived duplex execution is not a useful
unary JSON operation.

## Host setup

The Component has one construction path. The VM crate creates the Lua state,
constructs an empty Environment, selects external packages, and installs them
internally once for every RPC call. The host cannot replace that configuration.

```rust
use barracuda_vm_component::VmComponent;

let component = VmComponent::default();
// event_router.load(Box::new(component))?;
```

There is no RPC for installing libraries or changing sandbox policy. Scripts
can only access a library installed by the VM crate, for example
`local gpio = require("gpio")`.

The exact globals, data-flow functions, native-module policy, and current
resource-limit gaps are documented in [environment.md](environment.md).

## Fixed frame layout

All three wire messages are exactly 64 bytes and work with a 64-byte Event
Router lane.

`RunRequestFrame` contains:

- `kind: RunRequestKind` (`Source` or `Input`), one byte;
- `boundary: ChunkBoundary` (`More` or `Complete`), one byte;
- `text: VmTextChunk`, 62 bytes.

`RunResponseFrame` contains:

- `boundary: ChunkBoundary`, one byte;
- one reserved zero byte;
- `text: VmTextChunk`, 62 bytes.

`RunError` contains:

- `kind: RunErrorKind`, one byte;
- `diagnostic: VmErrorText`, 63 bytes.

Both text types are canonical NUL-terminated UTF-8 C strings. A request or
response chunk can carry at most 61 UTF-8 bytes; an error diagnostic can carry
at most 62. Bytes after the first NUL must be zero. Their Serde representation
is a JSON string, never an array of bytes. Long values must be split only at
UTF-8 character boundaries.

## Request state machine

1. Send one or more `Source` frames. `Source(..., Complete)` completes the Lua
   chunk and starts execution. An empty script is `Source("", Complete)`.
2. After execution starts, send zero or more logical input messages. Each
   message consists of one or more `Input` frames ending in `Input(...,
   Complete)`. One complete message satisfies one Lua `io.input()` call.
3. Close the request stream when no more input will arrive. If the final input
   has only `More` frames, EOF commits that accumulated value as the final input
   message. The input flow then closes; after queued messages are consumed,
   Lua `io.input()` returns `nil`.

`Input` before source completion, `Source` after execution starts, or EOF before
source completion terminates the call with `InvalidProtocol`.

The default limits are 65,536 source bytes and 4,096 bytes per logical input
message. A host can replace both with `VmLimits::new`.

## Response and completion

Every Lua `io.print(...)` produces one logical output message. A long message is
split across `RunResponseFrame`s; `Complete` marks its final frame. An empty
printed message is represented by one empty `Complete` frame.

The response stream is also the execution handle:

- response EOF means the script finished successfully;
- a terminal `RunError` means execution failed;
- response frames already produced by Lua are delivered before its terminal
  load/runtime error;
- an outer `RpcError` is transport failure, not a Lua or protocol error.

Dropping the response stream cancels the RPC and drops its `LuaExecution`.
Cancellation is cooperative: sandbox policy must arrange periodic Lua yield
points if CPU-bound or looping scripts need bounded cancellation latency.

## Method errors

| Kind | Meaning |
| --- | --- |
| `InvalidProtocol` | Request order or EOF violated the state machine. |
| `InvalidText` | A fixed text field was not canonical NUL-terminated UTF-8. |
| `SourceLimitExceeded` | Complete source exceeded the configured byte limit. |
| `InputLimitExceeded` | One logical input message exceeded its byte limit. |
| `VmCreate` | The VM crate failed while creating Lua. |
| `VmConfigure` | VM configuration or execution IO setup failed. |
| `LuaLoad` | Lua could not load the source chunk. |
| `LuaRuntime` | Lua or a native binding failed while executing. |
| `UnexpectedYield` | Lua yielded outside the wrapper's async protocol. |
| `OutputEncoding` | Printed text could not be represented by the frame format. |

Diagnostics are bounded and may be UTF-8-truncated. Callers must branch on
`RunErrorKind`, not diagnostic text.
