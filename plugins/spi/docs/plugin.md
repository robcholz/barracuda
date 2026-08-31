# SPI Plugin

- Plugin ID: `spi`
- Direct Plugin dependencies: `vm`
- Required typed capability: `barracuda_vm_package_api::LuaPackageRegistry` from `vm`
- Provided typed capabilities: none

During unified Plugin registration, this Plugin registers the require-only
`spi` Lua package with the VM registry. Bus names are the logical names
explicitly exposed by the selected Board.

Lua API:

- `spi.available(name) -> boolean`
- `spi.read(name, length) -> binary string`
- `spi.write(name, binary_string)`
- `spi.transfer(name, write_binary_string, read_length) -> binary string`
- `spi.transfer_in_place(name, binary_string) -> binary string`

The Board adapter owns chip-select policy, bus configuration, and transaction
serialization. Adapter failures use the conventional Lua `nil, error` result.
