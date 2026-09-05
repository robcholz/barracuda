# Boards

`boards/` owns concrete product bundles, reusable built-in peripheral Drivers,
and the build-time tooling that turns their common YAML into static Rust data.
Platform-specific Board HAL code may live inside its owning Platform bundle.

```text
boards/
|-- api/          # no_std `Board`, `Hardware`, and `Storage` values
|-- config/       # std-only YAML parsing, validation, and Rust generation
|-- drivers/      # reusable semantic built-in peripheral Drivers
`-- configs/
    `-- <board>/
        |-- board.yml
        `-- <Platform-native layout files>
```

One directory under `configs/` describes one concrete product. `board.yml`
contains the maximum common denominator: identity, canonical chip name, the
explicit Board hardware surface, and native-layout binding. It must not contain
a Rust Platform type or select `macos`, `linux`, `esp32`, or another Platform
implementation.

The optional hardware surface has two sections. `exposed-io` names only the
digital GPIO, analog, PWM, I2C, and SPI capabilities made available above the
Board layer. `builtin-peripherals` names a Driver, its chip-native bindings,
and typed construction parameters. Repeating a physical identifier is accepted
because overlap and mux behavior belong to the concrete Board adapter.

```yaml
exposed-io:
  gpio:
    user-control:
      pin: gpio2
  i2c:
    expansion:
      peripheral: i2c0
      scl: gpio6
      sda: gpio7
      frequency-hz: 400000

builtin-peripherals:
  indicator:
    driver: gpio-indicator
    bindings:
      pin: gpio10
    parameters:
      active-low: true
```

A Board that declares this surface must declare its concrete `board-hal`
Cargo package, workspace-relative path, and exported type in `board.yml`. The
path may point inside `platforms/<platform>/boards/`; no central selected-crate
registry is edited. The build rejects hardware declarations that would
otherwise be silently reduced to the empty HAL.

Physical layout remains in that Board bundle but uses the boot ecosystem's
native format:

- ESP32: `partitions.csv` (including OTA slots and ESP flags).
- STM32: `memory.x` and linker/Embassy Boot symbols.
- macOS and Linux: `file-layout.yml` for its file-backed NOR image.

Those native files are the only physical source of truth. Platform build code
validates `board.yml` bindings against them and projects the selected region
into `embedded-storage`/Embassy capabilities. Do not add a generic Barracuda
partition table or a converter that emits vendor tables.

Select a concrete Board once, then use the ordinary build command:

```bash
cargo board select
cargo run
```

`cargo board select` opens a colored, fuzzy-searchable list and defaults to the
currently selected Board. It validates the complete bundle before writing the
ignored workspace-local Board, Platform, and Cargo selection files. Scripts
may pass an explicit name as `cargo board select <board-name>`. The command also
updates the selected dependency blocks, so native Cargo sees a complete static
dependency graph before compilation starts. A build with no selection stops
with the command needed to select one; it never guesses a Board from the Rust
target.

`boards/selected` does not infer a Board from the target OS or architecture.
The selection command resolves the Board's chip and toolchain target against
the self-described Platform catalog, then generates the two selected axes
independently. Device entry code constructs the exact typed Platform and Board
bindings.

Adding a Board bundle or HAL is a maintainer operation. Run `cargo board sync`
and commit its deterministic workspace dependency block. Use
`cargo board sync --check` in CI to reject a stale registry. Consumers who pull
that commit only run `cargo board select` followed by ordinary Cargo commands.

The repository currently provides reference Board bundles for the ESP32,
ESP32-S2, ESP32-S3, ESP32-C3, ESP32-C6, and ESP32-P4 Platforms. The catalog
also includes Espressif DevKitM boards (`esp32c3-devkitm-1`,
`esp32c6-devkitm-1`, `esp32s3-devkitm-1`), the ESP32-S2-Kaluga and
ESP32-S3-BOX-3 evaluation kits, and the M5Stamp C3 Mate
(`m5stack-stamp-c3-mate`). Each ESP bundle keeps its ESP-IDF partition CSV
as the native layout and records the chip-specific Rust target in
`board.yml`.

Boards that use the same module are still separate Board bundles: add their
exact fixed wiring to that bundle as the Board schema grows rather than treating
one development kit as an alias for every product built around the chip.

`stm32f429zi-nucleo` is the first bundle with a registered concrete Board HAL.
Its config builds the active-high green LD1 on PB0 as the semantic
`IndicatorLed` builtin and explicitly exposes the PC13 user-button pin as the
runtime-configurable GPIO name `user-button`. The pins follow the default
Nucleo-144 solder-bridge setup; the common schema does not infer or reserve
either resource.

## M5Stack Board coverage

Barracuda provides selectable bundles for M5Stack controllers built on every
ESP chip family currently implemented by the workspace: ESP32, ESP32-C3,
ESP32-C6, ESP32-S3, and ESP32-P4. Run `cargo board select` and search for the
`m5stack-` prefix to see the complete, current catalog.

These bundles establish Board identity, the correct Rust compilation target,
and a native ESP partition layout. They are the platform and storage bring-up
baseline; product-specific displays, touch controllers, sensors, audio devices,
and other fixed peripherals still require their corresponding Board matrix and
HAL Driver composition before those capabilities are exposed to System or
Plugins. ESP32-C5 and ESP32-H2 products are intentionally excluded until those
Platforms exist in Barracuda.
