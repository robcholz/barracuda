# Virtual I/O

Host-only virtual GPIO and I2C hardware shared by the Linux and macOS
Platforms. It never enters firmware.

- **Platform HAL.** `barracuda_platform_virtual_io::hal` is re-exported as
  the `hal` module of both host Platforms. It provides the virtual peripheral
  singleton (`VirtualPeripherals`: pins `GPIO0`..`GPIO15`, I2C controllers
  `I2C0`, `I2C1`, and `I2C2`), the `RuntimePlatform` adapter behind the
  Board's exposed I/O, and the `i2c-device` constructors for peripheral
  implementations. The host Boards expose `vio-0`..`vio-7` on
  `GPIO0`..`GPIO7` and leave `I2C0` and `I2C1` to runtime I/O.
- **Board-declared peripherals.** The host Boards declare peripherals in
  `board.yml` like any Board. The host Platforms set `peripheral-models` in
  their HAL manifest, so generated Board composition calls
  `hal::declare_peripheral` for each one before initializing any; the HAL
  attaches the chip model for its implementation at the declared bus and
  address (see below), and the System runs the real chip driver and
  peripheral implementation against it.
- **Virtual peripherals manager.** One process-wide `VirtualHardware` holds
  pin state, I2C buses with attached device models, fault rules, and a
  timestamped event log. It listens on the loopback address in
  `BARRACUDA_VIRTUAL_IO_ADDR`. The variable is required: a host System without
  it, or with a non-loopback or unparsable value, exits at startup.
- **Device models.** `I2cDevice` implementations own register behaviour and
  may report datasheet order and timing violations. They are independent of
  the manager and the System, so a chip driver test can run the driver over a
  standalone `VirtualHardware` with a manual `Clock` and `VirtualDelay`.

## Board-declared peripherals

`declared` maps a peripheral implementation to its chip model, placed on the
controller of the implementation's `i2c` binding at its `address` parameter:

| Implementation | Model | Initial state |
| --- | --- | --- |
| `rx8130ce-rtc` | `rx8130ce` | kept time on its backup supply: VLF = 0, the clock running from the host's UTC time (initial power-on outside 2000–2099) |
| `ina226-power-monitor` | `ina226` | power-on state, measuring a 5.000 V bus and 200 mA through the declared `shunt-micro-ohms` |
| `bq27220-power-monitor` | `bq27220` | power-on state |

A peripheral without a model is logged and uses its bindings directly. The
host Boards declare `real-time-clock` (RX8130CE at `32h`) and `power-monitor`
(INA226 at `40h`, 5 mΩ shunt) on `I2C2` (`GPIO8`/`GPIO9`, 400 kHz). Their
models are ordinary attached devices: the manager reads and writes their
registers, injects faults at their addresses, and records their
transactions and datasheet violations, which the System also logs as
warnings. `buses` names the peripheral of each such device, and `reset`
keeps them.

## Pin model

| Mode | Resolved level |
| --- | --- |
| output, push-pull | the output latch |
| output, open-drain | low when the latch is low or the line is driven low; otherwise high |
| input | the externally driven level; undriven: high with pull-up, otherwise low |
| disabled | the externally driven level; undriven: low |

An I2C claim marks SCL and SDA as open-drain outputs held high. Every change
of a pin's mode or resolved level is recorded with its time and source
(`system` for HAL calls, `manager` for control requests).

## I2C model

A transaction addressed to an absent device fails with an address NACK.
Otherwise the device receives its phases in bus order: adjacent writes are one
phase (no repeated START between them), and each read phase is filled by the
device. The `registers` model has 256 bytes and an 8-bit pointer: a write
phase sets the pointer from its first byte and stores the remaining bytes, a
read phase returns bytes from the pointer, and both auto-increment and wrap.

Fault rules apply per bus or per address before the device is addressed, the
first matching rule wins, and a `once` rule is removed after it fires:

| Fault | `embedded-hal` error kind |
| --- | --- |
| `nack` | `NoAcknowledge(Address)` |
| `arbitration-loss` | `ArbitrationLoss` |
| `timeout` | `Other` (debug text `Timeout`) |
| `bus-error` | `Bus` |

## Control protocol

TCP on the configured loopback address. Each request is one JSON object on
one line; each response is one JSON object on one line, in request order.
Successful responses carry `"ok": true` and the fields below; failures carry
`"ok": false` and an `"error"` message. Pins are named by Board name (`vio-0`)
or chip name (`GPIO0`); buses by controller name (`I2C0`); byte strings are
hex without separators.

| `op` | Request fields | Response fields |
| --- | --- | --- |
| `pins` | | `pins`: every pin |
| `pin` | `pin` | `pin` |
| `set_input` | `pin`, `level`: `true`, `false`, or `null` to release | `pin` |
| `buses` | | `buses`: controllers, their pins and clock while open, and devices (with `peripheral` for a Board-declared one) |
| `add_device` | `bus`, `address`, `model` (default `registers`), `data` (optional initial bytes from offset 0) | |
| `remove_device` | `bus`, `address` | |
| `read_registers` | `bus`, `address`, `offset` (default 0), `length` | `data` |
| `write_registers` | `bus`, `address`, `offset` (default 0), `data` | |
| `inject_fault` | `bus`, `address` (optional), `fault`, `once` (default `true`) | `id` |
| `clear_faults` | `bus` (optional: every bus) | `cleared` |
| `faults` | | `faults` |
| `events` | `since` (default 0): first sequence number | `events` |
| `violations` | | `violations` |
| `reset` | | |

A pin is reported as `{name, chip, function, mode, pull, drive, output,
driven, level}`. Register access through the manager has no bus side effects
and is not recorded. `reset` removes devices, faults, external drives,
events, and violations; pin modes, output latches, controller claims, and
the devices behind Board-declared peripherals belong to the running System
and stay.

Events carry `seq`, `at_us` (microseconds since the manager started), and
`kind`:

```json
{"seq":3,"at_us":1520,"kind":"gpio","pin":"vio-0","mode":"output","level":true,"source":"system"}
{"seq":4,"at_us":1710,"kind":"i2c","bus":"I2C0","address":80,"operations":[{"write":"10"},{"read":"aabb"}],"result":"ok","injected":false}
```

A device model's notes follow its transaction as
`{"kind":"device","bus":"I2C0","address":67,"model":"pi4ioe5v6408","note":"P3 high"}`.
`result` is `ok`, `nack`, `arbitration-loss`, `timeout`, or `bus-error`. The
log keeps the latest 4096 events. A violation carries the `seq` of the
transaction that exposed it, `at_us`, `bus`, `address`, `model`, `rule`, and
`message`.

```console
$ printf '%s\n' '{"op":"add_device","bus":"I2C0","address":80}' \
    '{"op":"set_input","pin":"vio-2","level":true}' | nc -q1 127.0.0.1 7878
{"ok":true}
{"ok":true,"pin":{"name":"vio-2","chip":"GPIO2",...,"level":true}}
```

## Chip models

`models` holds datasheet models of real chips reached over I2C. Every
register behaviour, ordering rule, and timing number cites its datasheet
(document, revision, section or table) in the model source, and each rule
violation names that citation. Attach one through the manager with
`add_device` and its `model` name.

| `model` | Chip | Datasheet | Checks |
| --- | --- | --- | --- |
| `registers` | generic 256-byte register device | — | none |
| `ina226` | TI INA226 power monitor | TI SBOS547C (2011, rev. Aug 2026) | register set, two-byte access, read-only registers, conversion timing, Equations 3 and 4 |
| `rx8130ce` | Epson RX8130CE RTC | Epson ETM50E-10 | 30 ms power-on access wait, t_str before initial setting, user registers only, TEST bit, valid date, initialize all registers after VLF |
| `pi4ioe5v6408` | Diodes PI4IOE5V6408 I/O expander | Diodes DS40583 Rev 3-5 | register map, pin drive from direction/output/high-Z (noted as `P<n> high/low/released` events), no burst reads |
| `bq27220` | TI BQ27220 fuel gauge | TI SLUSCB7A, SLUUBD4A | 66 µs t(BUF) between packets, two commands per second, 250 ms tPUCD, read-only commands NACK |
| `es8311`, `es7210`, `es8389` | Everest audio converters | Everest ES8311 rev 10.0, ES7210 rev 21.0, ES8389 rev 5.0 | documented register map and defaults, one-byte write format, address write before every read |

Device models may also note device-side changes (such as an expander pin
changing level) as `device` events in the log.

## Virtual SPI

`VirtualHardware::with_spi` adds SPI controllers for driver tests;
`spi_bus(name, frequency_hz)` opens one at a fixed clock as an
`embedded-hal` `SpiBus` (blocking and async). Each bus operation is one
full-duplex transfer recorded as an `spi` event with its MOSI and MISO
bytes; MISO reads FFh without a device. `inject_fault` accepts an SPI
controller name (per bus, no address) and fails the next transfer. An
`SpiDevice` model receives each transfer with its start time and the bus
clock. The host Boards expose no SPI, so the control interface does not
attach SPI devices.

| SPI model | Chip | Datasheet | Checks |
| --- | --- | --- | --- |
| `Ws2812bModel` | Worldsemi WS2812B LED chain on MOSI | Worldsemi WS2812B (undated) | T0H/T0L/T1H/T1L ±150 ns from the MOSI bit stream and bus clock, RES latch above 50 µs, 24-bit GRB frames |

## Driver tests

```rust,ignore
let hardware = VirtualHardware::new(&[], &["I2C0"], Clock::manual());
hardware.attach("I2C0", 0x40, Box::new(MyChipModel::new()))?;
let bus = hardware.i2c_bus("I2C0")?;
let delay = VirtualDelay::new(hardware.clock().clone());
my_driver::init(bus, delay)?;
assert!(hardware.violations().is_empty());
```
