# SPI Plugin

- Plugin ID: `spi`
- Direct Plugin dependencies: `vm`
- Required typed capability: `barracuda_vm_package_api::LuaPackageRegistry` from `vm`
- Provided typed capabilities: none

During unified Plugin construction, this Plugin shares the Board's single
runtime I/O owner. During registration it installs the require-only `spi` Lua
package in the VM registry.

Lua API:

- `spi.open_bus(sck, mosi, miso, frequency_hz, mode) -> handle`
- `handle:read(length) -> binary string`
- `handle:write(binary_string)`
- `handle:transfer(write_binary_string, read_length) -> binary string`
- `handle:transfer_in_place(binary_string) -> binary string`
- `handle:is_open() -> boolean`
- `handle:close()`

Open requires SCK and at least one data pin, rejects duplicate signal roles,
and accepts SPI modes 0 through 3. The provider selects a compatible free
controller, claims it with the selected pins atomically, and returns an
`embedded_hal_async::spi::SpiBus`. The package defines no parallel SPI
operation trait. Controller identities are Platform details. This API owns a
raw bus and performs no chip-select operation; a single read is limited to
65,536 bytes. Configuration and HAL failures use the conventional Lua
`nil, error` result.

Explicit close, lexical `<close>`, userdata collection, and Lua-state teardown
drop the bus and invalidate later operations. Raw pin and controller tokens
remain consumed until restart because the generated owner does not recreate
vendor HAL singletons. Plugin revocation also invalidates existing handles.
