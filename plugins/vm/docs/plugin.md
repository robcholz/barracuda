# VM Plugin

- Plugin ID: `vm`
- Direct Plugin dependencies: none
- Provided typed capabilities: `barracuda_vm_package_api::LuaPackageRegistry`
- Required typed capabilities: none

The Plugin publishes `LuaPackageRegistry` during registration so dependent
Plugins can install require-only Lua packages. It loads `VmComponent` with the
same registry and an unstarted `VmRuntime`. During startup, `VmRuntime` receives
the System-owned Embassy spawner.

The Agent/Workflow-facing surface consists of the public JSON RPCs `vm.run`,
`vm.input`, and `vm.cancel`, plus the JSON Events `vm.output`,
`vm.input_required`, and `vm.finished`. There is no native RPC endpoint at
`vm.run` and no transport-level streaming cardinality.

Owned resources:

- `VmComponent` and its JSON registrations;
- `VmRuntime` and four statically allocated Embassy task slots;
- four reusable 64 KiB Lua allocator slots backed by
  `embedded_alloc::TlsfHeap`;
- a one-message input queue and one-message Lua output queue per execution;
- the built-in package plan, currently containing the require-only `io`
  package;
- the shared `LuaPackageRegistry` capability.

Each accepted run receives one isolated Lua state and one task/memory slot.
Output Event delivery is awaited before the VM proceeds, and output is split
into bounded chunks. Unloading the Component detaches Event delivery, cancels
active executions, and closes their input channels; Plugin Manager remains
responsible for Component and registration teardown.
