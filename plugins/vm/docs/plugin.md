# VM Plugin

- Plugin ID: `vm`
- Direct Plugin dependencies: none

During registration, the VM Plugin selects its built-in Lua package installation
plan. During startup, it loads the standalone Lua VM Component with that plan.
The Component creates fresh package instances for each isolated execution and
exposes the VM RPC and Event contracts documented in this directory.

Owned resources:

- `VmComponent`
- the `BuiltinPackages` plan, currently containing the require-only `io` package

It owns the VM Component and does not provide or require a typed Plugin
capability.
