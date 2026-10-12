# Display Plugin

The Display Plugin owns every Board display no other Plugin claims and exposes
drawing and panel control to Lua code as the `display` Lua package.

- Plugin ID: `display`
- Dependencies: `vm`
- Settings: none
- Context resources: peripheral slot `display.rest` (`*`)
- Provided capabilities: none
- Required capabilities: `LuaPackageRegistry` from `vm`
- Workflow: none
- Agent Tools: none
- Lua packages: `display`
- Web: none
- Tasks: none
- Storage: none

Its `display.rest` slot receives, at build time, every display of the Board
that no other enabled Plugin's slot takes, under the Board's device names.
When another Plugin claims the primary display, Lua drives the others;
otherwise Lua gets the primary display too. Panel buses and
control pins stay inside the selected implementation. Portable RGB888 input is
converted by the implementation to its native LCD or e-paper pixel
representation.

Lua API:

- `display.available([name]) -> boolean`: whether the named display, or any
  display when no name is given, can still be opened
- `display.list() -> { name, ... }`: the Board names of the Plugin's displays
- `display.open([name]) -> handle`: opens the named display, or the first one
  still available
- `handle:descriptor() -> width, height, pixel_format, technology`
- `handle:draw_rgb888(x, y, width, height, rgb_bytes)`
- `handle:flush(mode)` where mode is `automatic`, `full`, `partial`, or `fast`
- `handle:set_power(state)` where state is `on`, `sleep`, or `off`
- `handle:set_brightness(value)` on the `0..255` scale
- `handle:set_orientation(value)` where value is `deg0`, `deg90`, `deg180`, or `deg270`
- `handle:wait_ready()`
- `handle:is_open() -> boolean`
- `handle:close()`

```lua
local display = require('display')
for _, name in ipairs(display.list()) do
  print(name, display.available(name))
end
local status <close> = display.open('status')
```

Boards that leave the Plugin no display report unavailable and return an empty
list. Region dimensions and payload lengths are checked before the
implementation is called. Each display is move-only, so it may be opened once
per boot.
