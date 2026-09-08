# Driver composition

## Overview

Barracuda separates reusable peripheral behavior from Board wiring. A Driver
owns one hardware implementation and produces a semantic capability. A Board
selects Drivers and supplies physical bindings and configuration through
`board.yml`. The build resolves those declarations against Driver-owned
`driver.yml` schemas and emits a monomorphized Board HAL.

There is no runtime Driver registry. The generated Board HAL contains concrete
Platform resources, Driver factories, and capability types.

## Responsibilities

The subsystem is split into four ownership boundaries:

- `drivers/api` owns stable, chip-independent Driver and built-in capability
  traits plus narrow camera-frame and PCM bindings missing from embedded-hal.
- Concrete directories under `drivers/` own protocol behavior and a
  `driver.yml` manifest.
- `drivers/config` discovers manifests, validates Board declarations, and
  renders the selected static HAL.
- Peripheral Drivers import standard digital, SPI, I2C, PWM, and delay traits
  directly from `embedded-hal`. Each concrete Platform's `hal` module constructs
  those values from vendor-HAL tokens.
- `boards/hal` owns the named resource envelope consumed by VM GPIO, I2C, and
  SPI Plugins. Its GPIO mode and analog extensions cover operations absent from
  embedded-hal without replacing upstream bus operations.

`board.yml` owns product wiring. It names a stable Driver ID, assigns physical
resources to Driver-defined roles, and supplies Driver-defined parameters. It
does not name a Cargo package, Rust crate, Rust type, or source path.

Protocol controllers reserved for built-ins live under `internal-io`. A named
`spi-device` contains the chip controller, signal pins, chip select, and
frequency. A data-only `spi-output`, such as a WS2812 waveform, declares only
its real physical data pin; generation deterministically reserves a compatible
controller from the Platform pool instead of inventing a clock pin in Board
wiring. Camera and I2S resources additionally size their statically allocated
DMA storage with `dma-buffer-bytes`; allocation is emitted at the concrete Board
call site, so it costs nothing when that data plane is not selected. These
resources are consumed by their Drivers and are never exposed through the
Board's public I/O surface.

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
- `led_strip::LedStrip` presents RGB pixels independently of WS2812 or APA102
  wire encoding.
- `camera::Camera` captures a described frame into caller-owned storage.
- `audio::AudioCodec` controls volume and transfers interleaved PCM samples.
- Their `Builtin*` ownership traits transfer each primary built-in capability
  to one consumer exactly once.

Display descriptors separate the concrete embedded-graphics color type from
portable metadata. They report LCD, OLED, e-paper, LED-matrix, or video-output
technology independently of transport. `DisplayFeatures` reports whether a
concrete Driver is buffered, supports partial refresh, brightness, orientation,
sleep, busy waiting, or readback.

## Driver manifests

Every selectable Driver has `drivers/<id>/driver.yml`. The directory and
manifest IDs must match. Version 1 manifests declare:

- stable Driver ID and produced capability ID;
- Cargo package, Rust crate, factory type template, binding expression, and
  configuration expression used only by generation;
- named binding roles, their hardware categories, and safe initial output
  levels;
- named parameter schemas, including required values, defaults, and enum
  domains, with optional Rust value mappings.

An optional digital-output binding represents a control signal that is not
physically connected on every Board variant. Generation emits `None` when the
Board omits it and `Some(move-only output)` when it is wired; Drivers never
invent placeholder GPIOs for reset or power-down signals marked NC.

`barracuda_driver_config::load_catalog` rejects malformed manifests, unsupported
API versions, invalid implementation identities, invalid defaults, and
directory mismatches. `resolve_board` rejects unknown Drivers, missing or
unknown bindings, missing or unknown parameters, wrong value types, and enum
values outside the Driver schema.

## Static composition flow

`cargo board sync` validates all Board and Driver declarations and maintains
workspace dependencies for discovered peripheral Drivers.

`cargo board select <board>` independently selects the Platform, validates its
declared HAL bindings, fully resolves and renders the Board, then writes only
selected peripheral Driver dependencies into `boards/selected/Cargo.toml`.
`boards/selected/build.rs` reloads the selected Board and Driver catalog,
resolves the composition, and writes concrete Rust to `OUT_DIR`.

For the STM32F429ZI GPIO backend, generated bindings contain only the peripheral
tokens named by the selected Board. The generated initializer converts PB0
into a digital output, initializes `IndicatorLedDriver` through
`PeripheralDriver`, converts PC13 into exposed dynamic GPIO, and returns
`BoardHalResources`. The former per-Board Nucleo HAL crate is not part of this
flow.

The ESP32 and ESP32-S3 Platform HAL modules perform the equivalent translation
for GPIO, SPI, and I2C. They turn Board-selected controller and pin tokens into
concrete `embedded-hal` values. Generated HALs construct every selected Driver
and retain its concrete capability in `GeneratedBuiltins`.

## Driver families

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

`ws2812-spi-led-strip` owns the three-bit SPI waveform encoding used by WS2812
and SK6812 RGB chains. `apa102-led-strip` owns APA102 and SK9822 framing. Both
produce the same LED-strip capability while consuming an exclusive SPI bus.

`ov2640-camera` owns sensor detection and the QVGA JPEG SCCB register program.
It consumes `embedded-hal` I2C for control and the narrow camera frame-receiver
binding for DMA-backed capture.

`ov3660-camera` owns the sensor's 16-bit SCCB register protocol, product-ID
validation, QVGA window/scaler/PLL setup, and complete JPEG framing validation.
It consumes the same frame-receiver binding as OV2640 without adding a sensor
case to Board generation or the ESP32-S3 Platform.

`es8311-audio-codec` owns codec reset, register initialization, external
amplifier enable, volume, and PCM sample conversion. It consumes `embedded-hal`
I2C and digital output traits for control plus a full-duplex PCM binding for the
data plane.

`es8389-audio-codec` owns the ES8389 slave-mode initialization, BCLK-derived or
external-MCLK clock selection, bias startup, dual-channel volume, and duplex
PCM behavior. Both codec Drivers consume the same interleaved signed 16-bit PCM
binding.

## Design decisions

### Barracuda owns capability semantics

Applications and Plugins depend on Barracuda's `Display` or `Indicator`, not a
controller crate. A custom Driver can implement the same traits without being
added to the API crate.

### Drivers own configuration schemas

A Board cannot silently pass arbitrary YAML to a constructor. The Driver
manifest is the compatibility boundary and produces an early error before
firmware compilation when wiring or parameters drift.

### Platform HAL owns vendor adaptation

Vendor token shapes and constructors are implemented beside each Platform and
shared by every compatible Board. Product-specific controller and pin choices
stay in `board.yml`, avoiding Board or chip HAL crates while retaining move-only
ownership.

Concrete peripheral Drivers depend on `embedded-hal` directly. Boundary tests
reject a Driver that depends on a vendor HAL or a Barracuda general-purpose HAL
facade. A selected Platform must declare every binding form required by the
Board; unsupported camera, I2S, or other data planes fail during composition
validation.

### Peripheral Drivers own static construction

The generator understands binding categories, substitutions, and ownership; it
does not match concrete Driver IDs. Each `driver.yml` supplies its factory,
bindings, and configuration templates. Adding an unknown peripheral therefore
requires its Driver crate and manifest plus Board YAML, without editing the
central renderer.

### Composition is compile-time only

Generated code uses associated types and direct trait calls. It introduces no
`dyn` capability object, heap allocation, string lookup, or runtime probe
registry on the steady-state path.
