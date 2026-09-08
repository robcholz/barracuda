# Analog Plugin

- Plugin ID: `analog`
- Direct Plugin dependencies: `vm`
- Required typed capability: `barracuda_vm_package_api::LuaPackageRegistry` from `vm`
- Provided typed capabilities: none

This Plugin shares the Board's unified exposed-I/O owner and installs the
`analog` Lua package. `embedded-hal` 1.0 does not define ADC or DAC contracts,
so Barracuda uses narrow raw-value input/output traits while leaving pin mux,
calibration, and converter construction in the Platform HAL.

Lua API:

- `analog.input_available(name) -> boolean`
- `analog.output_available(name) -> boolean`
- `analog.open_input(name) -> handle`
- `analog.open_output(name) -> handle`
- `input:max_value() -> integer`
- `input:read() -> integer`
- `output:max_value() -> integer`
- `output:write(value)`
- `handle:is_open() -> boolean`
- `handle:close()`

Opening consumes the physical pin from the same owner used by GPIO, I2C, and
SPI. Unsupported pin functions fail before the token moves. Closing a handle
drops the converter value without recreating the raw pin token.
