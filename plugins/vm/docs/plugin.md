# VM Plugin

- Plugin ID: `vm`
- Direct Plugin dependencies: none
- Provided typed capabilities: `VmPackageRegistry`

During registration, the VM Plugin selects its built-in Lua package
installation plan and explicitly loads the standalone Lua VM Component with
that plan and an unstarted `VmRuntime`. During Plugin startup, the runtime
receives the System-owned Embassy spawner. Each RPC execution then occupies
one slot in the Component's static four-task Embassy pool.

The Component creates fresh package instances for each isolated execution and
exposes the VM RPC and Event contracts documented in this directory. Its Lua
instruction hook only yields execution; the owning Embassy task performs the
100 ms async wait before polling Lua again.

The Plugin publishes `VmPackageRegistry` for dependent Plugins. A consumer
registers a per-execution package factory during its own registration phase and
retains the returned `VmPackageRegistration`. Every `vm.run` installs fresh
instances of those external packages after the VM-owned built-ins. Dropping the
registration removes the package factory. External packages therefore extend
the VM without becoming built-ins or creating a dependency from VM back to the
provider.

Owned resources:

- `VmComponent`
- `VmRuntime` and its four statically allocated Embassy task slots
- four reusable 64 KiB Lua allocator slots backed by `embedded_alloc::TlsfHeap`
- the `BuiltinPackages` plan, currently containing the require-only `io` package

It owns the VM Component and does not provide or require a typed Plugin
capability.
