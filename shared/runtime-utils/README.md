# barracuda-runtime-utils

Small, dependency-light helpers shared across the barracuda Rust crates:

1. **`Cancel` / `CancellationFlag`** — cooperative cancellation signals.
2. **`stream::StreamPart`** — shared `Delta`/`End` vocabulary for logical streams.
3. **`yield_stream`** — safe single-task async generator adapters.
4. **`define_prefixed_id!`** — the strongly typed, wire-prefixed id newtype macro.
5. **`define_id_allocator!`** — a monotonic allocator for a prefixed id type.

The crate name is `barracuda-runtime-utils`; the library is imported as `barracuda_runtime_utils`.

## `stream::StreamPart<T>` — logical stream parts

Use `StreamPart` when a larger event stream multiplexes a logical incremental
stream that needs its own explicit boundary:

```rust
use barracuda_runtime_utils::stream::StreamPart;

let fragment = StreamPart::Delta("hello");
let end: StreamPart<&str> = StreamPart::End;
```

A plain Rust `Stream` whose `None` is the only relevant boundary does not need
this wrapper.

## `yield_stream` — single-task async generators

`yield_stream` and `try_yield_stream` turn an async producer into a boxed
`Stream`. The producer receives a `Yielder` and suspends after every item until
the consumer takes it. The implementation uses `Rc<RefCell>` and is therefore
intentionally local (`!Send`) and suitable for one cooperative executor task.

## `define_prefixed_id!` — wire-prefixed id newtypes

Defines a `u32` newtype whose wire form carries a fixed string prefix
(`session-1`, `task-2`, …). This gives each domain id its own type — a `TaskId`
can't be passed where a `SessionId` is expected — while keeping a compact,
human-readable serialized form.

```rust
use barracuda_runtime_utils::define_prefixed_id;

define_prefixed_id!(SessionId, "session-", "session");
define_prefixed_id!(TaskId, "task-", "task");

let id = SessionId::new(1);
assert_eq!(id.to_wire(), "session-1");
assert_eq!(SessionId::from_wire("session-1").unwrap(), id);
```

Each generated type derives `Clone, Copy, Debug, PartialEq, Eq, Hash` and
implements:

- `new(u32)` / `From<u32>` — construct from the raw number.
- `to_wire()` / `Display` — render to the prefixed string.
- `from_wire(&str)` / `FromStr` — parse and validate the prefix.
- `Serialize` / `Deserialize` — (de)serialize **by the wire string**, so the
  prefix is part of the on-wire representation.

Parsing returns `IdParseError` (`Empty` or `Invalid { kind, value }`) when the
input is blank or lacks the expected prefix and numeric suffix. `parse_prefixed_id`
is exposed as the shared parsing primitive the macro builds on.

## Where it fits

`barracuda-runtime-utils` has no platform dependencies, so it compiles and
tests identically on device and host. Higher-level crates such as `barracuda_agent_runtime`
build their domain ids (`IterationId`, `TaskId`, `StepId`, `WorkerId`,
`SessionId`) with `define_prefixed_id!`.
