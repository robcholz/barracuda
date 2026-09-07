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

The mode-changing calls are intentionally dynamic. The Plugin takes a concrete
named resource set whose pin type implements `ConfigurableDigitalPin` and the
standard `embedded_hal::digital` traits. It adds no GPIO operation trait or
hardware trait object. The package wraps that set in an Embassy async mutex,
and every Lua operation holds the lock while mutably borrowing the selected
pin. Invalid names, unsupported electrical modes, and pin failures use the
conventional Lua `nil, error` result.

Dropping the package registration revokes callbacks already installed in Lua
states, so they cannot retain GPIO access after the Plugin unloads.
