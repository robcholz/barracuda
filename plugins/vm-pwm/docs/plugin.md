# PWM Plugin

- Plugin ID: `pwm`
- Direct Plugin dependencies: `vm`
- Required typed capability: `barracuda_vm_package_api::LuaPackageRegistry` from `vm`
- Provided typed capabilities: none

This Plugin shares the Board's unified exposed-I/O owner and installs the
`pwm` Lua package. The returned output implements
`embedded_hal::pwm::SetDutyCycle`; timer, channel, and pin-mux allocation remain
inside the Platform provider.

Lua API:

- `pwm.available(name) -> boolean`
- `pwm.open(name, frequency_hz) -> handle`
- `handle:max_duty() -> integer`
- `handle:set_duty(value)`
- `handle:set_percent(percent)`
- `handle:is_open() -> boolean`
- `handle:close()`

Opening consumes the selected physical pin from the same owner used by every
other exposed protocol. Unsupported routing fails before the token moves.
