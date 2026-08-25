# Time RPC API

The Time Component owns a network-synchronized real-time clock. It establishes
UTC from SNTP at startup, periodically resynchronizes, and uses the local
monotonic clock only to interpolate between accepted network samples.

## `time.now`

- Address: `time.now`
- Dynamic JSON: yes
- Request: unary `TimeNowRequest`, represented as `{}` in JSON
- Response: unary `TimeNow`
- Method error: `TimeRpcError`
- Request schema: baked at build time and embedded in the dynamic RPC metadata

The fixed-layout request and response DTOs live in `barracuda-time-wire`. Its
host-only `schema` feature derives and registers the `TimeNowRequest` schema;
the Component build script bakes that schema into `OUT_DIR`. The normal wire
crate remains `no_std` and does not ship `schemars` to the target.

`TimeNow` fields:

| Field | Type | Meaning |
| --- | --- | --- |
| `year` | `u16` | UTC year. |
| `month` | `u8` | UTC month, 1 through 12. |
| `day` | `u8` | UTC day of month. |
| `hour` | `u8` | UTC hour, 0 through 23. |
| `minute` | `u8` | UTC minute, 0 through 59. |
| `second` | `u8` | UTC second, 0 through 59. |

Method errors:

| Variant | Meaning |
| --- | --- |
| `Unsynchronized` | No valid network time sample has completed since boot. |
| `Stale` | The last accepted sample is older than the configured maximum holdover. |
| `OutOfRange` | The synchronized UTC value cannot be represented as calendar fields. |

The RPC never substitutes uptime, zero, or a firmware timestamp for synchronized
UTC. Callers must treat every method error as clock unavailable.
