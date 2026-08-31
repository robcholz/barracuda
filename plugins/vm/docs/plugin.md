# VM Plugin

- Plugin ID: `vm`
- Direct Plugin dependencies: none
- Provided typed capabilities: `barracuda_vm_package_api::LuaPackageRegistry`

During registration, the VM Plugin publishes `LuaPackageRegistry`, selects its
built-in Lua packages, and loads the standalone Lua VM Component with the same
registry and an unstarted `VmRuntime`. Plugins that depend on `vm` register
their require-only Lua packages into this capability during the unified Plugin
registration phase. During Plugin startup, the runtime receives the
System-owned Embassy spawner. Each RPC execution then occupies one slot in the
Component's static four-task Embassy pool and installs the packages in the
shared registry.

The Component creates fresh package instances for each isolated execution and
exposes the VM RPC and Event contracts documented in this directory. Its Lua
instruction hook only yields execution; the owning Embassy task performs the
100 ms async wait before polling Lua again.

Owned resources:

- `VmComponent`
- `VmRuntime` and its four statically allocated Embassy task slots
- four reusable 64 KiB Lua allocator slots backed by `embedded_alloc::TlsfHeap`
- the `BuiltinPackages` plan, currently containing the require-only `io` package
- the shared `LuaPackageRegistry` capability

It owns the VM Component, provides `LuaPackageRegistry`, and does not require a
typed Plugin capability.
