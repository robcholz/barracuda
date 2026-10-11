# LED Strip Plugin

Installs the `led_strip` Lua package over the Board's primary built-in LED strip.

- Plugin ID: `led-strip`
- Dependencies: `vm`
- Settings: none
- Context resources: `hal.peripherals.take_led_strip()`
- Provided capabilities: none
- Required capabilities: `LuaPackageRegistry` from `vm`
- Workflow: none
- Agent Tools: none
- Lua packages: `led_strip`
- Web: none
- Tasks: none
- Storage: none

This Plugin takes the Board's primary built-in `LedStrip` capability exactly
once and installs the `led_strip` Lua package. It consumes the semantic peripheral
capability and never exposes or reconstructs its SPI transport.

Lua API:

- `led_strip.available() -> boolean`
- `led_strip.open() -> handle`
- `handle:len() -> integer`
- `handle:write(rgb_bytes)` where each pixel is three consecutive RGB bytes
- `handle:clear()`
- `handle:is_open() -> boolean`
- `handle:close()`

Boards without a built-in LED strip still install the package; `available`
returns false and `open` returns `nil, error`. The capability is move-only, so
only one handle can be opened during a boot. Explicit close, lexical `<close>`,
collection, and Plugin revocation prevent further operations.
