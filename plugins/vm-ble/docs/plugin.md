# BLE Plugin

- Plugin ID: `ble`
- Direct Plugin dependencies: `vm`
- Required typed capability: `barracuda_vm_package_api::LuaPackageRegistry` from `vm`
- Provided typed capabilities: none
- Registered Workflow Actions: none
- Emitted Workflow Events: none
- Registered Agent Tools: none
- Owned tasks: none
- Storage responsibilities: none

This Plugin shares the Board HAL's unified runtime owner and installs the
require-only `ble` Lua package. The selected Platform HAL supplies one
platform-independent `BleAdapter`; the Plugin has no dependency on a vendor
radio or BLE stack.

The first version is a BLE Central observer: it exposes bounded advertisement
scanning. Connections, GATT discovery and operations, notifications, pairing,
and peripheral advertising remain outside this version so their ownership and
buffering contracts can be added without changing the scan API.

Lua API:

- `ble.available() -> boolean`
- `ble.open() -> adapter`
- `adapter:scan(active, timeout_millis) -> address, address_kind, rssi_dbm, connectable, payload`
- `adapter:is_open() -> boolean`
- `adapter:close()`

`scan` waits for one advertisement and returns five `nil` values when its
timeout expires. Addresses use colon-separated hexadecimal bytes, address kind
is `public` or `random`, and payload is a binary-safe Lua string. The timeout
must be between 1 and 60,000 milliseconds. Advertising data is bounded to the
legacy BLE limit of 31 bytes.

The adapter is move-only and can be opened once per boot. Explicit close,
lexical `<close>`, userdata collection, VM teardown, and Plugin revocation drop
the adapter and invalidate later operations. The HAL does not recreate the
underlying radio singleton after close.
