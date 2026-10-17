# Power Monitor Plugin

Installs the `power_monitor` Lua package over the Board's power monitor,
which the `power` Plugin owns.

- Plugin ID: `power-monitor`
- Dependencies: `vm`, `power`
- Settings: none
- Context resources: none
- Provided capabilities: none
- Required capabilities: `LuaPackageRegistry` from `vm`, `Power` from `power`
- Workflow: none
- Agent Tools: none
- Lua packages: `power_monitor`
- Web: none
- Tasks: none
- Storage: none

This Plugin installs the `power_monitor` Lua package over `Power`. The
monitor, its drivers, and the battery's charge for other Plugins belong to
the `power` Plugin.

Lua API:

- `power_monitor.available() -> boolean`
- `power_monitor.open() -> handle`
- `handle:measure() -> bus_microvolts, shunt_nanovolts, current_microamps, power_microwatts`
- `handle:is_open() -> boolean`
- `handle:close()`

`measure` reads one coherent sample. Bus voltage is positive; shunt voltage,
current, and power are signed, positive in the monitor's IN+ to IN- direction.
A failed measurement returns `nil, error`.

```lua
local power_monitor = require('power_monitor')
local monitor <close> = assert(power_monitor.open())
local bus_uv, shunt_nv, current_ua, power_uw = assert(monitor:measure())
print(string.format('%.3f V %.1f mA %.1f mW', bus_uv / 1e6, current_ua / 1e3, power_uw / 1e3))
```

Boards without a power monitor still install the package; `available` returns
false and `open` returns `nil, error`. One handle may be opened per boot;
explicit close, lexical `<close>`, collection, and Plugin revocation prevent
further operations on it.

A handle reads through the shared `Power`, so other Plugins read the
monitor while a handle is open. The Plugin performs no background sampling
and owns no long-running tasks.
