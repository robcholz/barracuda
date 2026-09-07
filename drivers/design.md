# Driver composition

## Overview

Barracuda separates reusable peripheral behavior from Board wiring. A Driver
owns one hardware implementation and produces a semantic capability. A Board
selects Drivers and supplies physical bindings and configuration through
`board.yml`. The build resolves those declarations against Driver-owned
`driver.yml` schemas and emits a monomorphized Board HAL.

There is no runtime Driver registry. The generated HAL contains concrete chip
tokens, concrete Driver factories, and concrete capability types.

## Responsibilities

The subsystem is split into four ownership boundaries:

- `drivers/api` owns stable, chip-independent Driver and built-in capability
  traits.
- Concrete directories under `drivers/` own protocol behavior and a
  `driver.yml` manifest.
- `drivers/config` discovers manifests, validates Board declarations, and
  renders the selected static HAL.
- `boards/chips/<chip>` translates vendor-HAL tokens into the binding
  categories consumed by Drivers. It does not contain product wiring.

`board.yml` owns product wiring. It names a stable Driver ID, assigns physical
resources to Driver-defined roles, and supplies Driver-defined parameters. It
does not name a Cargo package, Rust crate, Rust type, or source path.

Protocol controllers reserved for built-ins live under `internal-io`. A named
`spi-device` contains the chip controller, signal pins, chip select, and
frequency. This resource is consumed by its Driver and is never exposed through
the Board's public I/O surface.

## Stable APIs

`barracuda_driver::PeripheralDriver` is the common construction contract. A
Driver declares its move-only `Bindings`, validated `Config`, returned
`Capability`, and initialization `Error`. Initialization returns an unboxed
future and therefore remains statically dispatched.

Built-in capabilities are Barracuda-owned traits rather than concrete Driver
types:

- `indicator::Indicator` exposes semantic on/off state independently of the
  electrical active level. `IndicatorConfig` carries the Board's active level.
- `display::Display` extends `embedded_graphics_core::DrawTarget` and
  `OriginDimensions`. It adds descriptor, refresh, power, brightness,
  orientation, and readiness operations needed across direct LCD, buffered
  e-paper, and display-link implementations.
- `display::BuiltinDisplay` and `indicator::BuiltinIndicator` transfer the
  primary built-in capability to one consumer exactly once.

Display descriptors separate the concrete embedded-graphics color type from
portable metadata. They report LCD, OLED, e-paper, LED-matrix, or video-output
technology independently of transport. `DisplayFeatures` reports whether a
concrete Driver is buffered, supports partial refresh, brightness, orientation,
sleep, busy waiting, or readback.

## Driver manifests

Every selectable Driver has `drivers/<id>/driver.yml`. The directory and
manifest IDs must match. Version 1 manifests declare:

- stable Driver ID and produced capability ID;
- Cargo package, Rust crate, and public factory type used only by generation;
- named binding roles and their hardware categories;
- named parameter schemas, including required values, defaults, and enum
  domains.

`barracuda_driver_config::load_catalog` rejects malformed manifests, unsupported
API versions, invalid implementation identities, invalid defaults, and
directory mismatches. `resolve_board` rejects unknown Drivers, missing or
unknown bindings, missing or unknown parameters, wrong value types, and enum
values outside the Driver schema.

## Static composition flow

`cargo board sync` validates all Board and Driver declarations and maintains
workspace dependencies for discovered chip adapters and Drivers.

`cargo board select <board>` independently selects the Platform, then writes
only the selected chip adapter and Driver dependencies into
`boards/selected/Cargo.toml`. `boards/selected/build.rs` reloads the selected
Board and Driver catalog, resolves the composition, and writes concrete Rust to
`OUT_DIR`.

For the STM32F429ZI adapter, generated bindings contain only the peripheral
tokens named by the selected Board. The generated initializer converts PB0
into a digital output, initializes `IndicatorLedDriver` through
`PeripheralDriver`, converts PC13 into exposed dynamic GPIO, and returns
`BoardHalResources`. The former per-Board Nucleo HAL crate is not part of this
flow.

The ESP32 and ESP32-S3 adapters perform the equivalent translation for GPIO
and SPI. They turn a Board-selected controller plus chip-select into a
standards-compliant `embedded_hal::spi::SpiDevice`; concrete display Drivers do
not depend on `esp-hal`. Generated HALs construct the selected display Driver
and expose its concrete capability through `BuiltinDisplay`.

## Implemented Drivers

`indicator-led` is the reference digital-output Driver. It implements both the
stable `Indicator` capability and the generic `PeripheralDriver` factory.

`mipi-dbi-display` owns initialization and lifecycle control for MIPI DCS panels
supported by `mipidsi`. Its manifest accepts ILI9342C, ST7789, ST7735S, GC9107,
and GC9A01 controller selections. The concrete Driver owns the transport
interface, reset output, backlight output, and delay source; initializes the
selected model; and adapts drawing, sleep/wake, orientation, digital backlight,
and direct presentation to the stable `Display` capability. ESP32 and ESP32-S3
M5Stack Boards with direct backlights select this Driver entirely in YAML.

`gdeh0154d67-display` owns the 200 by 200 framebuffer and GDEH0154D67 panel
protocol. It implements full and quick refresh waveforms, busy waiting,
sleep/off, and Board power-hold control through the same stable `Display`
capability. M5Stack CoreInk and AirQ select it with different chips and wiring
but no Board-specific Rust HAL.

## Design decisions

### Barracuda owns capability semantics

Applications and Plugins depend on Barracuda's `Display` or `Indicator`, not a
controller crate. A custom Driver can implement the same traits without being
added to the API crate.

### Drivers own configuration schemas

A Board cannot silently pass arbitrary YAML to a constructor. The Driver
manifest is the compatibility boundary and produces an early error before
firmware compilation when wiring or parameters drift.

### Chip adapters replace per-Board Rust HALs

Vendor token shapes and constructors are shared by every Board using a chip.
Product-specific pin choices stay in `board.yml`, avoiding duplicated HAL
crates while retaining move-only ownership.

### Composition is compile-time only

Generated code uses associated types and direct trait calls. It introduces no
`dyn` capability object, heap allocation, string lookup, or runtime probe
registry on the steady-state path.
