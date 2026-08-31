# SPI Plugin

- Plugin ID: `spi`
- Direct Plugin dependencies: `vm`
- Required typed capability: `barracuda_vm_package_api::LuaPackageRegistry` from `vm`
- Provided typed capabilities: none

During unified Plugin construction, this Plugin takes the SPI value directly
from `PluginContext::hal.io`. During registration it installs the require-only
`spi` Lua package in the VM registry.

Lua API:

- `spi.available(name) -> boolean`
- `spi.read(name, length) -> binary string`
- `spi.write(name, binary_string)`
- `spi.transfer(name, write_binary_string, read_length) -> binary string`
- `spi.transfer_in_place(name, binary_string) -> binary string`

The Plugin takes a concrete named resource set whose bus type implements
`embedded_hal_async::spi::SpiBus`; it does not define a parallel SPI operation
trait. The package wraps that set in an Embassy async mutex. Every Lua
transaction holds the lock across the complete async HAL transaction, so
operations on the owned bus are serialized. This API exposes a raw bus and
therefore performs no chip-select operation; a Board must expose a separate
GPIO for application-managed chip select, while built-in devices should use a
Board-composed `SpiDevice`. HAL failures use the conventional Lua `nil, error`
result. A single read result is limited to 65,536 bytes so an untrusted script
cannot request an unbounded Rust-side allocation.

Dropping the package registration revokes callbacks already installed in Lua
states, so they cannot retain SPI access after the Plugin unloads.
