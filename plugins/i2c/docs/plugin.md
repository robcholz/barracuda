# I2C Plugin

- Plugin ID: `i2c`
- Direct Plugin dependencies: `vm`
- Required typed capability: `barracuda_vm_package_api::LuaPackageRegistry` from `vm`
- Provided typed capabilities: none

During unified Plugin registration, this Plugin registers the require-only
`i2c` Lua package with the VM registry. Controller names are the logical names
explicitly exposed by the selected Board.

Lua API:

- `i2c.available(name) -> boolean`
- `i2c.read(name, address, length) -> binary string`
- `i2c.write(name, address, binary_string)`
- `i2c.write_read(name, address, binary_string, read_length) -> binary string`

The Board adapter owns the actual controller and transaction serialization.
Adapter failures use the conventional Lua `nil, error` result.
