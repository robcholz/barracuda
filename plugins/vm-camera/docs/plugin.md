# Camera Plugin

- Plugin ID: `camera`
- Direct Plugin dependencies: `vm`
- Required typed capability: `barracuda_vm_package_api::LuaPackageRegistry` from `vm`
- Provided typed capabilities: none

This Plugin takes the Board's primary built-in `Camera` capability exactly
once and installs the `camera` Lua package. OV2640/OV3660 control buses,
parallel capture, and DMA remain owned by the implementation and Platform HAL.

Lua API:

- `camera.available() -> boolean`
- `camera.open() -> handle`
- `handle:descriptor() -> width, height, pixel_format`
- `handle:capture(capacity) -> bytes`
- `handle:is_open() -> boolean`
- `handle:close()`

Capture storage is caller-sized and bounded to 4 MiB. Boards without a camera
report unavailable. The move-only built-in capability permits one handle per
boot; explicit close, lexical `<close>`, collection, and Plugin revocation
prevent further operations.
