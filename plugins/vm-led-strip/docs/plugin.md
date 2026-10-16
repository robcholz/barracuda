# LED Strip Plugin

Installs the `light` Lua package over the Board's primary LED strip.

- Plugin ID: `led-strip`
- Dependencies: `vm`
- Settings: none
- Context resources: peripheral slot `led-strip.device` (`primary`, optional)
- Provided capabilities: none
- Required capabilities: `LuaPackageRegistry` from `vm`
- Workflow: none
- Agent Tools: none
- Lua packages: `light`
- Web: none
- Tasks: none
- Storage: none

This Plugin takes the Board's primary `LedStrip` capability exactly
once and installs the `light` Lua package. It consumes the semantic peripheral
capability and never exposes or reconstructs its SPI transport.

Lua API:

- `light.available() -> boolean`
- `light.open() -> handle`
- `handle:len() -> integer`
- `handle:write(rgb_bytes)` where each pixel is three consecutive RGB bytes
- `handle:clear()`
- `handle:is_open() -> boolean`
- `handle:close()`

The package is the app API's `light` capability
([`app-api.md`](../../app-manager/docs/app-api.md)) while the Board has the
device.

Boards without a built-in LED strip still install the package; `available`
returns false and `open` returns `nil, error`. The capability is move-only, so
only one handle can be opened during a boot. Explicit close, lexical `<close>`,
collection, and Plugin revocation prevent further operations.
