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
let packages = barracuda_vm_builtin_packages::BuiltinPackages::all();
let (input, output) = packages.install(&mut lua)?.into_io();
lua_package_registry.install(&mut lua)?;
let completion = lua.run(source);
```

During Plugin registration, the VM selects the complete built-in package plan
from `barracuda-vm-builtin-packages`. Each `vm.run` call applies that immutable
plan to its own Lua state. The VM Plugin also provides `LuaPackageRegistry`;
dependent Plugins register their packages into it during the same unified
Plugin registration phase, and every new Lua state installs those registered
packages. The built-in plan installs `io`, which provides:

- `require("io").input()` asynchronously waits for one complete input message.
  After the caller closes input and queued messages are consumed, it returns
  `nil`.
- `require("io").print(...)` converts each argument with the sandbox
  `tostring`, joins the values with tabs, and appends one complete message to
  the run's eventual completion output.

Installing the IO package creates no global `io`, `input`, or `print` aliases.

These functions are message flows, not process standard input or standard
output. They do not read a terminal, write a console, or grant access to host
file descriptors. The internal input and output queues each hold one complete
message and apply backpressure when full.

`Lua::run()` only executes the already-configured sandbox. It does not install
an Environment, packages, or any globals.

The typed `Vm` capability accepts owned source and input strings. It does not
impose a transport-derived length limit. Starting a run returns `VmRun`: its
`run_id` is available immediately for input and cancellation.
`VmRun::next_update()` reports `VmRunProgress::InputRequired` before waiting for
input and later returns `VmRunUpdate::Completed`; directly awaiting the handle
returns `VmRunCompletion` with ordered output and the terminal outcome. See
[action.md](action.md) for Workflow behavior.

## Native modules

`require` only searches the internal preload table populated through Rust
`register_lib`. Filesystem Lua modules, native shared libraries, and arbitrary
searchers are not supported. The `package` table is not exposed, and the
bootstrap entries for `_G` and `package` are removed from the loaded-module
cache, so `require("_G")` and `require("package")` fail.

The VM composes one built-in native package: `io`. It is our
message-based package, not Lua's filesystem and process-oriented standard
`io` library. It is require-only: no global `io` table is installed.

The `gpio`, `i2c`, and `spi` Plugins register the require-only modules of the
same names through `LuaPackageRegistry`. Their visible logical resource names
come only from the selected Board's explicit exposed-I/O config. Other module
names still fail unless another enabled Plugin registers them. VM-owned
built-ins belong in `plugins/vm/crates/builtin-packages`; optional Plugin-owned
packages belong in their owning Plugin and register through the VM capability.

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

Consequently, a script has no ambient filesystem, network, clock, process,
terminal, dynamic-code-loading, hardware-registry, metatable, or
debug-reflection capability. Hardware access is limited to the logical
resources explicitly exposed through the registered `gpio`, `i2c`, and `spi`
modules. Any other access must arrive through an explicitly registered Rust
module.

The full `debug` library is intentionally excluded because it can expose the
Lua registry, recover private upvalues such as `require`'s package state,
replace a function's `_ENV`, mutate protected metatables, and interfere with VM
execution hooks. A future traceback API should expose only formatted diagnostic
text rather than the `debug` table.

## Cooperative instruction yielding

Normal `VmPlugin` executions run in a statically allocated Embassy task pool.
Lua's count hook yields after each configured instruction interval; the hook
does not run a timer and does not expose one to Lua. After observing that
yield, the VM executor task performs `Timer::after_millis(100).await` and then
resumes polling the same Lua execution. The default interval is 10,000
instructions and the pool supports four concurrent executions.

The `Vm` capability uses `VmRuntime` and the task-owned instruction scheduler.
Normal system composition constructs both through `VmPlugin`.

## Lua memory pool

`VmRuntime` preallocates four reusable allocator slots, matching the four
Embassy VM task slots. Each slot uses an `embedded_alloc::TlsfHeap` with a
default 64 KiB backing buffer. Starting one execution leases one slot and
passes that external Rust allocator to Lua through `Lua::new_with_allocator`.
Lua allocation, reallocation, garbage collection, and state destruction all
use that allocator; dropping the Lua state returns the allocator slot to the
pool.

Exceeding the fixed per-execution Lua heap completes the execution with
`outcome: "error"` and `error: "lua_memory"`.
`VmRuntime::with_memory_bytes` can replace the default per-slot capacity when
constructing the runtime. The Lua heap limit does not include owned source and
input strings, native callback objects, or Embassy task storage.

`vm.cancel` is observed by a blocked input immediately or by CPU-bound Lua at
an instruction-hook boundary or after the current 100 ms task delay.

## Resource limits not yet implemented

The current environment is a capability sandbox with a bounded Lua heap. It
does not currently enforce:

- an instruction or CPU-time budget;
- a wall-clock deadline;

Instruction yielding is scheduling, not a hard budget: a looping script is
periodically suspended and resumed but is not automatically terminated. The
VM still has no total-instruction ceiling, CPU quota, or wall-clock timeout.
