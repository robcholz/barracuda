# GPIO Plugin

- Plugin ID: `gpio`
- Direct Plugin dependencies: `vm`
- Required typed capability: `barracuda_vm_package_api::LuaPackageRegistry` from `vm`
- Provided typed capabilities: none

During unified Plugin construction, this Plugin takes the GPIO value directly
from `PluginContext::hal.io`. During registration it installs the require-only
`gpio` Lua package in the VM registry.

Lua API:

- `gpio.available(name) -> boolean`
- `gpio.input(name, pull)` where pull is `none`, `up`, or `down`
- `gpio.output(name, initial_high, drive)` where drive is `push-pull` or `open-drain`
- `gpio.disable(name)`
- `gpio.read(name) -> boolean`
- `gpio.write(name, high)`

The mode-changing calls are intentionally dynamic. The Plugin wraps its taken
value in an Embassy async mutex, and every Lua operation holds that lock while
mutably borrowing the adapter. Invalid names, unsupported electrical modes,
and hardware failures use the conventional Lua `nil, error` result.
