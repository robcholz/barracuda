# Lua Execution Environment

This document describes the environment that exists today. It is a contract of
the current implementation, not a list of planned libraries.

## Lifetime and isolation

Every `vm.run` call creates a new Lua state. The state, its globals, loaded
modules, input queue, and output queue belong to that call and are dropped when
the script completes, fails, or is cancelled. Lua values are never shared
between calls. Capability Plugins may own state outside Lua: in particular,
`vm-filesystem` stores files in that Plugin's scoped VFS, whose `/data` and `/media`
mounts follow the normal VFS persistence lifecycle.

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
| `getmetatable`, `setmetatable` | Inspect and assign ordinary Lua metatables. Native userdata metatables are protected. |
| `rawequal`, `rawget`, `rawlen`, `rawset` | Bypass ordinary Lua metamethod dispatch for table operations. |
| `require` | Load a native package installed into this Lua state. |
| `string` | Lua's standard string manipulation library. |
| `table` | Lua's standard table manipulation library. |
| `math` | Lua's standard mathematical library. |
| `utf8` | Lua's standard UTF-8 library. |

Lua syntax and language primitives remain available: values, tables, functions,
closures, conditionals, loops, operators, and multiple return values do not
come from a standard-library table.

`math.random` and `math.randomseed` retain their standard call shapes, but the
sandbox replaces the no-argument seed with `(0, 0)`. This prevents Lua's native
default seeding path from returning the host clock and a state address. Callers
that need a varied sequence must provide explicit seed values from an approved
capability.

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
packages. The built-in plan installs a message-backed implementation of Lua's
standard stream I/O shape:

- `io.stdin`, `io.stdout`, and `io.stderr` are virtual stream handles. They do
  not expose host file descriptors.
- `io.read(...)` and `io.stdin:read(...)` support the standard line (`l`/`*l`),
  line-with-newline (`L`/`*L`), all (`a`/`*a`), number (`n`/`*n`), byte-count,
  and multiple-format forms. Reads wait asynchronously when more input is
  required and return `nil` after EOF.
- `io.lines(...)` and `io.stdin:lines(...)` iterate the virtual input stream.
- `io.write(...)`, `io.stdout:write(...)`, and `io.stderr:write(...)` append to
  the run output stream; `io.flush()` is a no-op success for virtual output.
- `io.input()` and `io.output()` return the current default stream. The built-in
  environment accepts only its original standard handles as setters.
- `print(...)` converts arguments with `tostring`, joins them with tabs, and
  writes a trailing newline.

As in standard Lua, `io` and `print` are globals, and `require("io")` returns
the same `io` table. The nonstandard former name `io.print()` is not installed.

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

`Vm::list()` returns the active executions in fixed runtime-slot order. Each
entry contains only its `run_id` and a `running` or `input_required` state;
completed, failed, and cancelled executions are omitted.

## Native modules

`require` resolves the already-loaded `string`, `table`, `math`, and `utf8`
standard libraries, then only searches the internal preload table populated
through Rust `register_lib`. Filesystem Lua modules, native shared libraries,
and arbitrary searchers are not supported. The `package` table is not exposed,
and the bootstrap entries for `_G` and `package` are removed from the
loaded-module cache, so `require("_G")` and `require("package")` fail.

The VM composes two built-in native packages: `io` and an initially empty
`os` table. `io` implements virtual standard streams over VM messages and
installs the standard global `io` and `print` names. The empty `os` table gives
capability Plugins a single standard table to extend. Neither built-in owns a
filesystem, clock, or process authority.

The separate `vm-filesystem` Plugin extends that same `io` table with `io.open`,
`io.tmpfile`, file handles, filename-aware `io.lines`, and file-backed default
input/output. It also installs only the filesystem-shaped `os.remove`,
`os.rename`, and `os.tmpname` functions. All paths go directly through the
Plugin's private `ScopedVfs` without vm-filesystem path rewriting. Paths remain
confined by the VFS namespace, and mount permissions still apply.

The `gpio`, `i2c`, `spi`, and `vm-http` Plugins register require-only modules
through `LuaPackageRegistry`. `vm-http` provides the bounded high-level
`require("http").request` operation without exposing sockets or the Platform
network stack. Hardware modules expose only logical resource names from the
selected Board's explicit exposed-I/O config. Other module names still fail
unless another enabled Plugin registers them. VM-owned built-ins belong in
`plugins/vm/crates/builtin-packages`; optional Plugin-owned packages belong in
their owning Plugin and register through the VM capability.

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

- `package` and `coroutine`;
- `io.popen` and all host process or terminal handles;
- `os.clock`, `os.execute`, `os.exit`, `os.getenv`, and `os.setlocale`;
- the `debug` library;
- `load`, `loadfile`, and `dofile`;
- `collectgarbage` and `warn`.

Consequently, a script has no ambient host filesystem, raw network stack,
process, terminal, dynamic-code-loading, hardware-registry, or debug-reflection
capability. Filesystem access exists only when `vm-filesystem` is enabled and
remains inside that Plugin's scoped VFS. UTC calendar access exists only through
`vm-time`; bounded outbound HTTP exists only through `vm-http`. Hardware access
is limited to the logical resources explicitly exposed through the registered
`gpio`, `i2c`, and `spi` modules. Any other access must arrive through an
explicitly registered Rust module.

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
default 96 KiB backing buffer. Starting one execution leases one slot and
passes that external Rust allocator to Lua through `Lua::new_with_allocator`.
Lua allocation, reallocation, garbage collection, and state destruction all
use that allocator; dropping the Lua state returns the allocator slot to the
pool.

Exceeding the fixed per-execution Lua heap completes the execution with
`outcome: "error"` and `error: "lua_memory"`.
`VmRuntime::with_memory_bytes` can replace the default per-slot capacity when
constructing the runtime. The Lua heap limit does not include owned source and
input strings, native callback objects, or Embassy task storage.

Run output is capped at 64 KiB and 1,024 lines. File reads and individual file
writes are capped at 32 KiB, and `vm-filesystem` allows at most 16 simultaneously
open handles per registered package. HTTP requests use a 2 KiB URL, at most 32
headers totaling 16 KiB, a 32 KiB request body, a 64 KiB response body, two
concurrent workspaces, and a 30 second deadline. These native limits are
independent of the Lua heap.

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
