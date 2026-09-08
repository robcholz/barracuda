# GPIO Plugin

- Plugin ID: `gpio`
- Direct Plugin dependencies: `vm`
- Required typed capability: `barracuda_vm_package_api::LuaPackageRegistry` from `vm`
- Provided typed capabilities: none

During unified Plugin construction, this Plugin shares the Board's single
runtime I/O owner. During registration it installs the require-only `gpio` Lua
package in the VM registry.

Lua API:

- `gpio.available(name) -> boolean`
- `gpio.open_input(name, pull) -> handle` where pull is `none`, `up`, or `down`
- `gpio.open_output(name, initial_high, drive) -> handle` where drive is
  `push-pull` or `open-drain`
- `handle:read() -> boolean`
- `handle:write(high)`
- `handle:is_open() -> boolean`
- `handle:close()`

Open acquires an exclusive digital claim from a provider on the unified I/O
owner. The returned pin implements `ConfigurableDigitalPin` and the standard
`embedded_hal::digital` traits. Invalid names, busy pins, unsupported
electrical modes, and pin failures use the conventional Lua `nil, error`
result. Explicit close, lexical `<close>`, userdata collection, and Lua-state
teardown all disable and drop the handle. The generated device owner preserves
the vendor HAL's move-only token rule, so the physical pin remains claimed
until restart rather than being unsafely reconstructed after close.

Dropping the package registration revokes callbacks and invalidates operations
through handles already installed in Lua states.
