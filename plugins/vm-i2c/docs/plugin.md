# I2C Plugin

- Plugin ID: `i2c`
- Direct Plugin dependencies: `vm`
- Required typed capability: `barracuda_vm_package_api::LuaPackageRegistry` from `vm`
- Provided typed capabilities: none

During unified Plugin construction, this Plugin shares the Board's single
runtime I/O owner. During registration it installs the require-only `i2c` Lua
package in the VM registry.

Lua API:

- `i2c.open(scl, sda, frequency_hz) -> handle`
- `handle:read(address, length) -> binary string`
- `handle:write(address, binary_string)`
- `handle:write_read(address, binary_string, read_length) -> binary string`
- `handle:is_open() -> boolean`
- `handle:close()`

Open validates distinct SCL/SDA pins and a positive frequency, then asks the
provider to select a compatible free controller and claim it with both pins
atomically. Controller identities are Platform details, not Lua or Board pin
names. The returned bus implements `embedded_hal_async::i2c::I2c`; the Plugin
defines no parallel bus operation trait. Addresses are seven-bit and one read
is limited to 65,536 bytes. Configuration and HAL failures use the conventional
Lua `nil, error` result.

Explicit close, lexical `<close>`, userdata collection, and Lua-state teardown
drop the bus and invalidate later operations. Raw pin and controller tokens
remain consumed until restart because the generated owner does not recreate
vendor HAL singletons. Plugin revocation also invalidates existing handles.
