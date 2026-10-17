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
- `light.fill(color) -> true` sets every pixel to `color`, `"#RRGGBB"` or
  one of `white`, `warm`, `red`, `green`, `blue`, `off`, `black`
- `light.off() -> true` turns every pixel off
- `handle:len() -> integer`
- `handle:write(rgb_bytes)` where each pixel is three consecutive RGB bytes
- `handle:clear()`
- `handle:is_open() -> boolean`
- `handle:close()`

The package is the app API's `light` capability
([`app-api.md`](../../app-manager/docs/app-api.md)) while the Board has the
device.

`fill` and `off`, app API level 2, use the strip while no handle holds it
and return `nil, error` while one does.

Boards without a built-in LED strip still install the package; `available`
returns false and `open` returns `nil, error`. One handle is open at a time:
explicit close, lexical `<close>`, and collection prevent further operations
on it and give the strip back, so `open` works again; Plugin revocation
prevents every further operation.
