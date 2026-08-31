# GPIO Plugin

- Plugin ID: `gpio`
- Direct Plugin dependencies: `vm`
- Required typed capability: `barracuda_vm_package_api::LuaPackageRegistry` from `vm`
- Provided typed capabilities: none

During unified Plugin registration, this Plugin registers the require-only
`gpio` Lua package with the VM registry. It uses only the logical GPIO names
explicitly exposed by the selected Board through `PluginContext`.

Lua API:

- `gpio.available(name) -> boolean`
- `gpio.input(name, pull)` where pull is `none`, `up`, or `down`
- `gpio.output(name, initial_high, drive)` where drive is `push-pull` or `open-drain`
- `gpio.disable(name)`
- `gpio.read(name) -> boolean`
- `gpio.write(name, high)`

The mode-changing calls are intentionally dynamic. The Board adapter owns the
actual pin driver and reports invalid names, unsupported electrical modes, or
hardware failures through the conventional Lua `nil, error` result.
