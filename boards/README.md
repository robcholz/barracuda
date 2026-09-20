# Boards

`boards/` owns concrete product bundles and the build-time tooling that turns
their common YAML into static Rust data. Semantic peripheral implementations live
under `peripherals/impl`; reusable low-level chip drivers live under
`drivers/chips`. Each Platform adapts its vendor HAL
into upstream hardware traits; Board bundles contain no handwritten Rust HAL.

```text
boards/
|-- api/          # no_std `Board`, `Hardware`, and `Storage` values
|-- config/       # std-only YAML parsing, validation, and Rust generation
|-- hal/          # named resource ownership exposed to hardware Plugins
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

The hardware surface has exactly two sections. `peripherals` describes optional
attached devices and the private I/O used to construct them. `exposed-io` names
physical resources that applications may configure at runtime. The Platform
HAL later consumes an exposed pin token as digital, analog, PWM, I2C, SPI,
UART, I2S, or another supported function selected by the application.

```yaml
peripherals:
  io:
    i2c-device:
      sensor-i2c:
        peripheral: I2C0
        scl: GPIO1
        sda: GPIO2
        frequency-hz: 400000
    spi-device:
      display-spi:
        peripheral: SPI2
        sck: GPIO6
        mosi: GPIO5
        chip-select: GPIO7
        frequency-hz: 80000000
  devices:
    display:
      implementation: mipi-dbi-display
      bindings:
        backlight: GPIO9
        dc: GPIO4
        reset: GPIO8
        spi: display-spi
      parameters:
        controller: gc9a01
        width: 240
        height: 240

exposed-io:
  pins:
    expansion-1: { pin: GPIO3 }
    expansion-2: { pin: GPIO4 }
```

Every selectable peripheral implementation owns
`peripherals/impl/<peripheral>/<implementation-id>/peripheral.yml`, which declares its
bindings, parameters, peripheral API, and static construction templates.
`cargo board sync` discovers implementations by convention. `board.yml`
remains hardware data and contains no Cargo package, crate path, Rust type, or
Platform feature registration. Selection resolves and renders the complete
composition before it writes selection state, then enables only the concrete
peripheral implementation packages. GPIO, SPI, I2C, camera-capture, and I2S-stream
construction comes from the independently selected Platform HAL.
The generated `SelectedBoardHal` is monomorphized; no runtime registry or trait
object is added.

The Board HAL does not replace `embedded-hal`. It provides one shared runtime
owner for exposed physical resources. Function providers atomically claim pins
and an internally selected controller when required, ask the selected Platform
to configure their mux, and return
concrete `embedded-hal` or `embedded-hal-async` values to `vm-gpio`, `vm-i2c`,
`vm-spi`, or another hardware Plugin. Its construction, GPIO-mode, and analog
extensions cover operations that embedded-hal 1.0 does not define.

Generated device owners preserve vendor HAL move-only semantics. One function
claim consumes its raw pin and controller tokens for the current boot; closing
the VM handle stops access and drops the constructed HAL value but does not
invent replacement singleton tokens. A conflicting or repeated open returns a
runtime error, and a restart rebuilds the complete exposed surface.

Physical layout remains in that Board bundle but uses the boot ecosystem's
native format:

- ESP32: `partitions.csv` (including OTA slots and ESP flags).
- STM32: `memory.x` and linker/Embassy Boot symbols.
- macOS and Linux: `file-layout.yml` for its file-backed NOR image.

Those native files are the only physical source of truth. Platform build code
validates `board.yml` bindings against them and projects the selected region
into `embedded-storage`/Embassy interfaces. Do not add a generic Barracuda
partition table or a converter that emits vendor tables.

Select a concrete Board once, then use the ordinary build command:

```bash
cargo board select
cargo run
```

`cargo run` owns only the selected System application. Start the external
terminal Channel in another terminal with `cargo cli [ws://DEVICE_ADDRESS:8787]`.
The CLI is forced to the development host target, so it can connect while an
embedded Board remains selected.

The application binary itself is always `barracuda-system`. Its tracked
`main.rs` is `no_std` and owns Target construction plus the System lifecycle.
It passes that application callback to the selected Platform's compile-time
entry macro, so host process startup and embedded firmware startup remain
Platform-owned without introducing another application package or runtime
dispatch.

`cargo board select` opens a colored, fuzzy-searchable list and defaults to the
currently selected Board. It validates the complete bundle before writing the
ignored workspace-local Board, Platform, and Cargo selection files. Scripts
may pass an explicit name as `cargo board select <board-name>`. The command
writes ignored local dependency packages under `.barracuda/selection`, so
native Cargo sees a complete static dependency graph without modifying tracked
selector manifests or `Cargo.lock`. It verifies and activates the resolved
Platform's rustup toolchain and writes its optional `build-std` and
installer-generated environment configuration, keeping all later checks,
builds, and runs on ordinary Cargo commands without shell setup. When the
toolchain is unavailable, selection reports the Platform-owned installation
prompt without changing the selected Board. A build with no selection stops
with the command needed to select one; it never guesses a Board from the Rust
target.

`boards/selected` does not infer a Board from the target OS or architecture.
The selection command resolves the Board's chip and toolchain target against
the self-described Platform catalog, then generates the two selected axes
independently. Device entry code constructs the exact typed Platform and Board
bindings.

Adding a Board bundle or peripheral implementation is a maintainer operation. Run `cargo board sync`
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

`stm32f429zi-nucleo` demonstrates the same YAML-only composition outside the
ESP family. Its config builds the active-high green LD1 on PB0 through the
shared `indicator-led` implementation and explicitly exposes PC13 as the dynamic GPIO
name `user-button`. The STM32 Platform HAL replaces its former Board-specific
HAL crate. The name `user-button` identifies the physical PC13 pin; an
application acquires its digital function at runtime.

## M5Stack Board coverage

Barracuda provides selectable bundles for M5Stack controllers built on every
ESP chip family currently implemented by the workspace: ESP32, ESP32-C3,
ESP32-C6, ESP32-S3, and ESP32-P4. Run `cargo board select` and search for the
`m5stack-` prefix to see the complete, current catalog.

The M5Stack Tab5 selects `m5stack-tab5`. Its Board HAL instantiates the onboard
ES8388/ES7210 audio path, BMI270 inertial measurement, INA226 power monitor,
RX8130CE real-time clock, microSD storage, revision-aware touch and DSI display,
with pure `esp-hal` bindings. Audio, display, and touch implementations
privately compose the PI4IOE5V6408 chip driver for their enable or reset lines;
the expander is not a System-visible peripheral. The Board also exposes
HY2.0-4P PORT.A, rear M5-Bus, and available SIT3088 RS-485 signals through the
shared runtime I/O owner. The
`m5stack-tab5x` identity remains separate and must not be substituted based only
on an ESP32-P4 silicon revision reported by the flashing tool.

The fitted SC202CS camera remains absent from the Board peripheral set because
`esp-hal` does not currently expose the ESP32-P4 MIPI-CSI/ISP data plane. It
must not be advertised until a real bare-metal driver can own that hardware.

All bundles establish Board identity, the correct Rust compilation target, and
a native ESP partition layout. The display matrix currently composes
ILI9342C, ST7789, and GC9A01 SPI LCDs through `mipi-dbi-display`, plus the
GDEH0154D67 e-paper panels in CoreInk and AirQ through
`gdeh0154d67-display`. Both families expose Barracuda's stable `DisplayPeripheral`
API. Other fixed peripherals remain absent until their reusable implementation is
implemented and selected in that Board's YAML. ESP32-C5 and ESP32-H2 products
are intentionally excluded until those Platforms exist in Barracuda.
