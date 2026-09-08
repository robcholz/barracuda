# Display Plugin

- Plugin ID: `display`
- Direct Plugin dependencies: `vm`
- Required typed capability: `barracuda_vm_package_api::LuaPackageRegistry` from `vm`
- Provided typed capabilities: none

This Plugin takes the Board's primary built-in `Display` capability exactly
once and installs the `display` Lua package. Panel buses and control pins stay
inside the selected Driver. Portable RGB888 input is converted by the Driver
to its native LCD or e-paper pixel representation.

Lua API:

- `display.available() -> boolean`
- `display.open() -> handle`
- `handle:descriptor() -> width, height, pixel_format, technology`
- `handle:draw_rgb888(x, y, width, height, rgb_bytes)`
- `handle:flush(mode)` where mode is `automatic`, `full`, `partial`, or `fast`
- `handle:set_power(state)` where state is `on`, `sleep`, or `off`
- `handle:set_brightness(value)` on the `0..255` scale
- `handle:set_orientation(value)` where value is `deg0`, `deg90`, `deg180`, or `deg270`
- `handle:wait_ready()`
- `handle:is_open() -> boolean`
- `handle:close()`

Boards without a display report unavailable. Region dimensions and payload
lengths are checked before the Driver is called. The built-in capability is
move-only, so one handle may be opened per boot.
