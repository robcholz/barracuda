# Lua Execution Environment

This document describes the environment that exists today. It is a contract of
the current implementation, not a list of planned libraries.

## Lifetime and isolation

Every `vm.run` call creates a new Lua state. The state, its globals, loaded
modules, input queue, and output queue belong to that call and are dropped when
the script completes, fails, or is cancelled. Nothing in the Lua environment is
shared or persisted across calls.

`Lua::new()` has one construction mode: an allowlist sandbox. There is no full
standard-library mode, no feature flag that disables the sandbox, and no host
factory that replaces it. Every loaded chunk receives the sandbox table as its
`_ENV`; `_G` refers to that same table.

Scripts may create or replace globals inside their own `_ENV`. Those changes
remain local to that one Lua state.

## Globals available after `Lua::new()`

| Global | Purpose |
| --- | --- |
| `_G` | The sandbox environment itself. |
| `_VERSION` | Lua version string. |
| `assert`, `error` | Raise Lua errors. |
| `pcall`, `xpcall` | Run a function with Lua error capture. |
| `pairs`, `ipairs`, `next` | Iterate tables. |
| `select` | Select variadic arguments. |
| `tonumber`, `tostring`, `type` | Basic value conversion and inspection. |
| `require` | Load a native package installed into this Lua state. |

Lua syntax and language primitives remain available: values, tables, functions,
closures, conditionals, loops, operators, and multiple return values do not
come from a standard-library table.

## Execution data flow

Creating the sandbox and installing the execution environment are separate
operations:

```rust,ignore
let mut lua = Lua::new()?;
let (io, input, output) = barracuda_lua_io::Io::new();
Environment::new()
    .with_package(io)
    .install(&mut lua)?;
let completion = lua.run(source);
```

The Lua crate provides an empty `Environment` builder and the `Package` trait.
The VM selects the external `barracuda-lua-io` package and adds it with
`with_package`. That package provides:

- `require("io").input()` asynchronously waits for one complete input message.
  After the caller closes input and queued messages are consumed, it returns
  `nil`.
- `require("io").print(...)` converts each argument with the sandbox
  `tostring`, joins the values with tabs, and emits one complete output
  message.

Installing the IO package creates no global `io`, `input`, or `print` aliases.

These functions are message flows, not process standard input or standard
output. They do not read a terminal, write a console, or grant access to host
file descriptors. Both internal queues hold up to 16 complete messages and
apply backpressure when full.

`Lua::run()` only executes the already-configured sandbox. It does not install
an Environment, packages, or any globals.

At the `vm.run` RPC boundary, source and messages are transported in bounded
frames. The default complete-source limit is 65,536 UTF-8 bytes and the default
limit for one logical input message is 4,096 UTF-8 bytes. See [rpc.md](rpc.md)
for framing, completion, and error behavior.

## Native modules

`require` only searches the internal preload table populated through Rust
`register_lib`. Filesystem Lua modules, native shared libraries, and arbitrary
searchers are not supported. The `package` table is not exposed, and the
bootstrap entries for `_G` and `package` are removed from the loaded-module
cache, so `require("_G")` and `require("package")` fail.

The VM currently composes one external native package: `io`. It is our
message-based package, not Lua's filesystem and process-oriented standard
`io` library. It is require-only: no global `io` table is installed. Calls such
as `require("gpio")`, `require("time")`, or `require("net")` still fail today.
Future capabilities are separate crates under `packages/` and are selected with
additional `with_package` calls.

Repeated `require` calls for a registered module return the cached module table
for that Lua state.

## Native functions

Registered Rust functions may be synchronous or asynchronous. An asynchronous
native function suspends and resumes the Lua execution internally; Lua code
does not receive an executor or a coroutine handle.

The Rust return contract is:

| Rust return | Lua result |
| --- | --- |
| `None` | No returned values. |
| `Some(Ok(values))` | The converted Lua values. |
| `Some(Err(error))` | `nil, error`. |

## Capabilities not present

The following Lua standard-library surfaces are not available:

- `package`, `coroutine`, `string`, `table`, `math`, and `utf8`;
- the standard Lua `io` library, plus `os` and `debug`;
- `load`, `loadfile`, and `dofile`;
- `collectgarbage` and `warn`;
- `getmetatable`, `setmetatable`, `rawequal`, `rawget`, `rawlen`, and `rawset`.

Consequently, a script currently has no direct filesystem, network, clock,
process, terminal, dynamic-code-loading, hardware, registry, metatable, or
debug-reflection capability. Such access must arrive through an explicitly
registered Rust module.

The full `debug` library is intentionally excluded because it can expose the
Lua registry, recover private upvalues such as `require`'s package state,
replace a function's `_ENV`, mutate protected metatables, and interfere with VM
execution hooks. A future traceback API should expose only formatted diagnostic
text rather than the `debug` table.

## Resource limits not yet implemented

The current environment is a capability sandbox, not yet a complete resource
sandbox. It does not currently enforce:

- a Lua heap or total allocation limit;
- an instruction or CPU-time budget;
- a wall-clock deadline;
- periodic preemption of CPU-bound Lua code.

Cancellation is cooperative. It is observed at wrapper-managed yield points,
including `io.input`, `io.print` backpressure, and asynchronous
native functions. A pure Lua infinite loop that never reaches such a point
cannot currently be cancelled with bounded latency.
