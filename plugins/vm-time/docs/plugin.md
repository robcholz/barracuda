# VM Time Plugin

- Plugin ID: `vm-time`
- Direct Plugin dependencies: `time`, `vm`
- Required typed capabilities: `barracuda_time_plugin::UtcClock` from `time`, `barracuda_vm_package_api::LuaPackageRegistry` from `vm`
- Provided typed capabilities: none
- Owned tasks: one cancellable UTC request bridge

This Plugin projects the Time Plugin's network-synchronized, locally advanced
UTC clock into Lua's standard `os` table. It owns no clock, network source, or
timezone configuration. Its bridge task preserves the executor-local
`UtcClock` ownership while the registered Lua package remains `Send + Sync`.

## Lua API

The VM's local timezone is fixed to UTC. The Plugin provides:

- `os.time()` for the current integer Unix timestamp in seconds;
- `os.time(table)` for a UTC calendar table, including normalization of
  out-of-range month, day, hour, minute, and second fields;
- `os.date([format [, timestamp]])`, including `*t`, `!*t`, the portable C
  conversion specifiers, and `!` UTC prefixes;
- `os.difftime(second, first)`.

Because local time is UTC, formats with and without `!` have identical results,
`%z` is `+0000`, `%Z` is `UTC`, and `isdst` is always false. A date format is
limited to 256 UTF-8 bytes.

`os.time()` and an `os.date` call without an explicit timestamp fail when the
Time Plugin has not synchronized yet or its holdover is stale. Explicit
timestamp conversion remains available because it requires no clock read.

`os.clock` is deliberately absent: it represents process CPU time, not UTC,
and the sandbox does not expose a timing side channel or host process clock.
Process, environment, locale, and exit functions also remain absent.
