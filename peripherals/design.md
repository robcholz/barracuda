# Peripheral composition

## Overview

Barracuda separates semantic peripheral APIs, peripheral implementations, and
low-level chip drivers. A Board has exactly two public hardware surfaces:
`peripherals` and `exposed-io`. Everything used only to construct a peripheral,
including GPIO expanders and bus switches, remains private to its implementation.

Composition happens at build time. The selected Board HAL contains concrete
types and `Option<T>` fields; firmware performs no runtime catalog lookup and
uses no peripheral trait objects.

## Ownership boundaries

- `peripherals/api` defines chip-independent semantic contracts such as
  `Display`, `Touch`, `PowerMonitor`, and `RealTimeClock`, plus the common
  `PeripheralImplementation` construction trait.
- `peripherals/impl/<peripheral>/<implementation-id>` adapts one concrete
  hardware arrangement to one semantic API. It owns its `peripheral.yml`,
  initialization policy, and private helper-chip dependencies.
- `peripherals/config` discovers only peripheral implementations, validates
  their manifests and Board wiring, and generates static Board composition.
- `drivers/chips/<chip>` contains low-level register and protocol drivers.
  Chip drivers know neither Board names nor semantic peripheral APIs and have
  no `peripheral.yml`.
- `boards/hal` owns the `BoardResources { peripherals, exposed_io }` envelope
  and the resource adapters used by generated composition.

The dependency direction is one-way:

~~~text
Platform HAL resource -> chip driver -> peripheral implementation -> peripheral API
                                  Board wiring -----------^
~~~

A peripheral implementation may use no separate chip crate when an upstream
ecosystem crate already provides the low-level driver. The semantic boundary is
still the same: System receives only the peripheral API.

## Board model

`board.yml` names product wiring, not Rust modules:

~~~yaml
peripherals:
  io:
    i2c-device:
      internal:
        peripheral: I2C0
        scl: GPIO32
        sda: GPIO31
        frequency-hz: 400000
  devices:
    battery-monitor:
      implementation: ina226-power-monitor
      bindings:
        i2c: internal
      parameters:
        address: 65

exposed-io:
  pins:
    port-a-sda: { pin: GPIO53 }
~~~

`peripherals.io` declares private resources consumed during construction.
`peripherals.devices` selects semantic implementations. `exposed-io` is the
only runtime-configurable Board I/O surface. A chip-native token cannot be both
private peripheral wiring and exposed I/O.

Every generated semantic slot is optional. A Board with no real-time clock has
no `RealTimeClock` value; a Board with one has `Some(concrete_clock)`. Absence
does not require fake hardware, and presence does not expose helper chips.

## Implementation manifests

Every selectable implementation owns
`peripherals/impl/<peripheral>/<implementation-id>/peripheral.yml`. The parent
directory equals `peripheral`, and the leaf directory equals `id`. Version 1
manifests declare:

- the stable implementation ID and semantic peripheral API;
- Cargo and Rust construction templates used only by code generation;
- named hardware binding roles and safe output initialization policies;
- typed parameters, defaults, enum domains, and Rust value mappings.

`peripherals/config::PERIPHERAL_APIS` is the registration boundary. An unknown
semantic kind is rejected with an instruction to add its API trait before its
first implementation. The generator understands binding categories and
ownership but contains no match on concrete implementation IDs.

`cargo board sync` validates every manifest and maintains workspace dependencies.
`cargo board select <board>` resolves the selected Board against those schemas,
checks Platform HAL support, and writes only the selected implementation
dependencies to `boards/selected/Cargo.toml`.

## Private helper chips

Helper chips are implementation details unless they independently provide a
semantic peripheral that System consumes. An I/O expander used for reset,
enable, interrupt, or power-control signals is therefore not automatically a
Board peripheral.

On M5Stack Tab5, the PI4IOE5V6408 at `0x43` is hidden this way:

- the audio-codec implementation owns the speaker-amplifier enable pin;
- the display implementation owns the panel reset pin;
- the touch implementation owns the touch reset pin;
- the camera implementation owns the sensor reset pin.

All four use `drivers/chips/pi4ioe5v6408`; none exposes the expander, its pins,
or a generic system-controller peripheral to System. Other PI4-controlled
signals must likewise be owned by the concrete semantic service they enable.

## Stable construction and runtime behavior

`PeripheralImplementation` declares move-only `Bindings`, validated `Config`,
the returned semantic `Peripheral`, and its initialization `Error`. Its future
is unboxed and monomorphized.

Platform HAL modules convert vendor tokens into `embedded-hal`,
`embedded-hal-async`, or narrow data-plane contracts such as DSI, MIPI-CSI,
camera capture, and PCM. Peripheral implementations depend on those contracts,
not vendor HALs. Chip drivers normally depend only on upstream protocol traits.

For DSI displays, the Platform binding establishes the link and transmits raw
DBI commands, the chip driver owns the controller command sequence, and the
display implementation selects timing and applies the required delays. This
keeps controller registers out of both the semantic API and the Platform HAL.

Generated code constructs each selected implementation directly and stores its
semantic result in `GeneratedPeripherals`. System and Plugins consume those
semantic values and never reconstruct implementations from raw pins.

## Adding hardware

Adding a Board that reuses existing implementations normally requires only
`board.yml` wiring and native layout artifacts. Adding a new chip behind an
existing semantic peripheral adds a chip driver when needed, one peripheral
implementation, and its manifest. Adding a genuinely new semantic peripheral
starts with its API trait and optional ownership slot, then adds implementations.

This keeps Board count from multiplying Rust code: reusable chip behavior,
semantic adaptation, Platform resource construction, and product wiring each
live in one layer.
