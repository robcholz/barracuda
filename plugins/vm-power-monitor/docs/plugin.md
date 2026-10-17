# Power Monitor Plugin

Installs the `power_monitor` Lua package over the Board's primary
power monitor, and publishes the battery's charge to other Plugins as
`PowerStatus`.

- Plugin ID: `power-monitor`
- Dependencies: `vm`
- Settings: none
- Context resources: peripheral slot `power-monitor.device` (`primary`, optional)
- Provided capabilities: `PowerStatus`
- Required capabilities: `LuaPackageRegistry` from `vm`
- Workflow: none
- Agent Tools: none
- Lua packages: `power_monitor`
- Web: none
- Tasks: none
- Storage: none

This Plugin takes the Board's primary `PowerMonitor` capability
exactly once and installs the `power_monitor` Lua package. I2C ownership, chip
initialization, calibration, and conversion to integer SI subunits remain owned
by the selected implementation and generated Board HAL.

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

`PowerStatus::battery()` reads the battery's charge (`BatteryState` with
`percent` and `charging`) through `PowerMonitor::battery`, which a battery
fuel gauge such as the BQ27220 answers and other monitors answer with
`None`. The Plugin keeps the monitor, so the capability reads it while a Lua
handle is open too. The Plugin performs no
background sampling and owns no long-running tasks.
