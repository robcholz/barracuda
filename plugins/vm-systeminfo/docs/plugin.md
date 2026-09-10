# VM System Info Plugin

- Plugin ID: `vm-systeminfo`
- Direct Plugin dependencies: `vm`
- Required typed capability: `barracuda_vm_package_api::LuaPackageRegistry` from `vm`
- Provided typed capabilities: none
- Workflow Actions: none
- Workflow Events: none
- Agent Tools: none
- Owned long-running tasks: none
- Storage: none
- Retained registrations: one `systeminfo` Lua package registration

This Plugin copies the selected Target's fixed `TargetIdentity` during System
construction and projects it into every Lua execution. Platform manifests own
Platform identity, Board configuration owns Board identity, and Target
composition preserves both independently selected axes.

## Lua API

~~~lua
local info = require("systeminfo")

print(info.platform.name)
print(info.platform.family)
print(info.platform.architecture)
print(info.platform.environment)
print(info.board.name)
print(info.board.chip)
~~~

The module contains only static values compiled into the selected image. Each
Lua state receives its own table, so script mutations remain local to that
execution. The package performs no hardware discovery, runtime sampling, or
background work.
