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
- `camera.snap([capacity]) -> bytes, pixel_format` takes one frame, with room
  for 128 KiB unless given
- `handle:descriptor() -> width, height, pixel_format`
- `handle:capture(capacity) -> bytes`
- `handle:is_open() -> boolean`
- `handle:close()`

The package is the app API's `camera` capability
([`app-api.md`](../../app-manager/docs/app-api.md)) while the Board has the
device.

`snap`, app API level 2, uses the camera while no handle holds it and
returns `nil, error` while one does.

Capture storage is caller-sized and bounded to 512 KiB in bulk memory. Boards
without a camera report unavailable. One handle is open at a time: explicit
close, lexical `<close>`, and collection prevent further operations on it and
give the camera back, so `open` works again; Plugin revocation prevents every
further operation.
