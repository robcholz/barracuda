# I2C Plugin

- Plugin ID: `i2c`
- Direct Plugin dependencies: `vm`
- Required typed capability: `barracuda_vm_package_api::LuaPackageRegistry` from `vm`
- Provided typed capabilities: none

During unified Plugin construction, this Plugin takes the I2C value directly
from `PluginContext::hal.io`. During registration it installs the require-only
`i2c` Lua package in the VM registry.

Lua API:

- `i2c.available(name) -> boolean`
- `i2c.read(name, address, length) -> binary string`
- `i2c.write(name, address, binary_string)`
- `i2c.write_read(name, address, binary_string, read_length) -> binary string`

Addresses in this package are seven-bit I2C addresses. The Plugin takes a
concrete named resource set whose controller type implements
`embedded_hal_async::i2c::I2c`; it does not define a parallel bus operation
trait. The package wraps that set in an Embassy async mutex. Every Lua
transaction holds the lock across the complete async HAL transaction, so
operations on the owned controller are serialized. HAL failures use the
conventional Lua `nil, error` result. A single read result is limited to 65,536
bytes so an untrusted script cannot request an unbounded Rust-side allocation.

Dropping the package registration revokes callbacks already installed in Lua
states, so they cannot retain I2C access after the Plugin unloads.
