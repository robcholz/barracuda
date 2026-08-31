# I2C Plugin

- Plugin ID: `i2c`
- Direct Plugin dependencies: `vm`
- Required typed capability: `barracuda_vm_package_api::LuaPackageRegistry` from `vm`
- Provided typed capabilities: none

During unified Plugin registration, this Plugin registers the require-only
`i2c` Lua package with the VM registry. It takes exclusive ownership of the I2C
value explicitly exposed and adapted for Lua by the selected Target.

Lua API:

- `i2c.available(name) -> boolean`
- `i2c.read(name, address, length) -> binary string`
- `i2c.write(name, address, binary_string)`
- `i2c.write_read(name, address, binary_string, read_length) -> binary string`

The Plugin wraps its taken value in an Embassy async mutex. Every Lua
transaction holds that lock while mutably borrowing the adapter, so async
operations on the owned controller are serialized. Adapter failures use the
conventional Lua `nil, error` result.
