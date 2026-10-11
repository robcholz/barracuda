# IMU Plugin

Installs the `imu` Lua package over the Board's primary built-in IMU.

- Plugin ID: `imu`
- Dependencies: `vm`
- Settings: none
- Context resources: `hal.peripherals.take_imu()`
- Provided capabilities: none
- Required capabilities: `LuaPackageRegistry` from `vm`
- Workflow: none
- Agent Tools: none
- Lua packages: `imu`
- Web: none
- Tasks: none
- Storage: none

This Plugin takes the Board's primary built-in `Imu` capability exactly once
and installs the `imu` Lua package. I2C ownership, chip initialization, integer
unit conversion, and Board-relative axis mapping remain owned by the selected
implementation and generated Board HAL.

Lua API:

- `imu.available() -> boolean`
- `imu.open() -> handle`
- `handle:descriptor() -> accelerometer_rate_hz, gyroscope_rate_hz, accelerometer_range_mg, gyroscope_range_mdps, has_temperature`
- `handle:read() -> acceleration_x_mg, acceleration_y_mg, acceleration_z_mg, angular_velocity_x_mdps, angular_velocity_y_mdps, angular_velocity_z_mdps, temperature_mc`
- `handle:is_open() -> boolean`
- `handle:close()`

Boards without an IMU report unavailable. The move-only built-in capability
permits one handle per boot; explicit close, lexical `<close>`, collection, and
Plugin revocation prevent further operations. The Plugin performs no
background sampling and owns no long-running tasks.
