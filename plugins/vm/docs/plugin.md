# VM Plugin

- Plugin ID: `vm`
- Direct dependency: `workflow`
- Provided typed capabilities: `Vm`, `LuaPackageRegistry`
- Required typed capabilities: `WorkflowActionRegistry` from `workflow`
- Plugin-owned tasks: one lifecycle task and up to four Lua execution tasks
- Workflow Actions: `vm.run`, `vm.input`, `vm.cancel`

`Vm` is the single typed execution API used by both Workflow Actions and the
separate `agent-vm` adapter. Starting a run returns an awaitable `VmRun` handle
whose identifier is available immediately. `VmRun::next_update` exposes typed
non-terminal progress and terminal completion, while awaiting `VmRun` directly
returns only the ordered output messages and terminal outcome.
Dropping an incomplete `VmRun` requests cooperative cancellation and wakes a
run that is waiting for input.

`Vm::list` returns a bounded snapshot of active run IDs and their current
`running` or `input_required` state without exposing Lua source.

The `vm.run` Workflow Action awaits that handle and returns only after the Lua
execution finishes. VM output and terminal state are not emitted as Workflow
Events, and input progress does not alter the Workflow Action contract. The
Plugin owns a fixed pool of four Embassy execution slots and four
reusable 96 KiB Lua heaps. Requests and responses use ordinary Serde values;
there is no Event Router lane or transport-derived source/input limit.

Dependent VM package Plugins still register require-only Lua packages through
`LuaPackageRegistry` during Plugin registration.
