# VM Plugin

- Plugin ID: `vm`
- Direct dependency: `workflow`
- Provided typed capabilities: `Vm`, `LuaPackageRegistry`
- Required typed capabilities: `WorkflowActionRegistry` from `workflow`
- Owned Components: none
- Plugin-owned tasks: one lifecycle task and up to four Lua execution tasks
- Workflow Actions: `vm.run`, `vm.input`, `vm.cancel`

`Vm` is the single typed execution API used by both Workflow Actions and the
separate `agent-vm` adapter. Starting a run returns an awaitable `VmRun` handle
whose identifier is available immediately. Awaiting it returns the ordered
output messages and terminal outcome.

The `vm.run` Workflow Action awaits that handle and returns only after the Lua
execution finishes. VM output and terminal state are not emitted as Workflow
Events. The Plugin owns a fixed pool of four Embassy execution slots and four
reusable 64 KiB Lua heaps. Requests and responses use ordinary Serde values;
there is no Event Router lane or transport-derived source/input limit.

Dependent VM package Plugins still register require-only Lua packages through
`LuaPackageRegistry` during Plugin registration.
