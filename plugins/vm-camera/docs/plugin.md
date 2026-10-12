# Camera Plugin

The Camera Plugin owns the Board's primary camera and exposes frame
capture to Lua code as the `camera` Lua package.

- Plugin ID: `camera`
- Dependencies: `vm`
- Settings: none
- Context resources: peripheral slot `camera.device` (`primary`, optional)
- Provided capabilities: none
- Required capabilities: `LuaPackageRegistry` from `vm`
- Workflow: none
- Agent Tools: none
- Lua packages: `camera`
- Web: none
- Tasks: none
- Storage: none

This Plugin takes the Board's primary `Camera` capability exactly
once and installs the `camera` Lua package. OV2640/OV3660 control buses,
parallel capture, and DMA remain owned by the implementation and Platform HAL.

Lua API:

- `camera.available() -> boolean`
- `camera.open() -> handle`
- `handle:descriptor() -> width, height, pixel_format`
- `handle:capture(capacity) -> bytes`
- `handle:is_open() -> boolean`
- `handle:close()`

Capture storage is caller-sized and bounded to 512 KiB in bulk memory. Boards without a camera
report unavailable. The move-only built-in capability permits one handle per
boot; explicit close, lexical `<close>`, collection, and Plugin revocation
prevent further operations.
