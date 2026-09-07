# VM Plugin

- Plugin ID: `vm`
- Direct dependency: `workflow`
- Provided capabilities: `Vm`, `LuaPackageRegistry`
- Workflow Actions: `vm.run`, `vm.input`, `vm.cancel`
- Workflow Events: `vm.output`, `vm.input_required`, `vm.finished`

`Vm` is the single typed execution API used by both Workflow Actions and the
separate `agent-vm` adapter. The Plugin owns four Embassy execution tasks and
four reusable 64 KiB Lua heaps. Requests and Events use ordinary Serde values;
there is no Event Router lane or transport-derived source/input limit.

Dependent VM package Plugins still register require-only Lua packages through
`LuaPackageRegistry` during Plugin registration.
