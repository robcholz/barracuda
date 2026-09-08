# UART Plugin

- Plugin ID: `uart`
- Direct Plugin dependencies: `vm`
- Required typed capability: `barracuda_vm_package_api::LuaPackageRegistry` from `vm`
- Provided typed capabilities: none

This Plugin shares the Board's unified exposed-I/O owner and installs the
`uart` Lua package. A port implements `embedded_io_async::Read + Write`;
controller allocation and pin routing remain inside the Platform provider.

Lua API:

- `uart.available(tx, rx) -> boolean`
- `uart.open(tx, rx, baud, data_bits, parity, stop_bits) -> handle`
- `handle:read(max_length) -> bytes`
- `handle:write(bytes)`
- `handle:flush()`
- `handle:is_open() -> boolean`
- `handle:close()`

`tx` or `rx` may be `nil`, but not both. Data bits are `7`, `8`, or `9`;
parity is `"none"`, `"even"`, or `"odd"`; stop bits are `1` or `2`.
Opening atomically consumes every selected physical pin from the same owner
used by the other exposed protocols.
