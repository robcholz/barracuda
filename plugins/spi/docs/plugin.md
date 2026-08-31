# SPI Plugin

- Plugin ID: `spi`
- Direct Plugin dependencies: `vm`
- Required typed capability: `barracuda_vm_package_api::LuaPackageRegistry` from `vm`
- Provided typed capabilities: none

During unified Plugin registration, this Plugin registers the require-only
`spi` Lua package with the VM registry. It takes exclusive ownership of the SPI
value explicitly exposed and adapted for Lua by the selected Target.

Lua API:

- `spi.available(name) -> boolean`
- `spi.read(name, length) -> binary string`
- `spi.write(name, binary_string)`
- `spi.transfer(name, write_binary_string, read_length) -> binary string`
- `spi.transfer_in_place(name, binary_string) -> binary string`

The Plugin wraps its taken value in an Embassy async mutex. Every Lua
transaction holds that lock while mutably borrowing the adapter, so async
operations on the owned bus are serialized. The adapter still defines
chip-select policy and bus configuration. Adapter failures use the conventional
Lua `nil, error` result.
