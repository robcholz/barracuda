# Boards

`boards/` owns concrete product bundles and the build-time tooling that turns
their common YAML into static Rust data.

```text
boards/
|-- api/          # no_std `Board`, `Hardware`, and `Storage` values
|-- config/       # std-only YAML parsing, validation, and Rust generation
`-- configs/
    `-- <board>/
        |-- board.yml
        `-- <Platform-native layout files>
```

One directory under `configs/` describes one concrete product. `board.yml`
contains the maximum common denominator: identity, canonical chip name, and
logical storage mappings. It must not contain a Rust Platform type or select
`macos`, `linux`, `esp32`, or another Platform implementation.

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
cargo build
```

`cargo board select` opens a colored, fuzzy-searchable list and defaults to the
currently selected Board. It validates the complete bundle before writing the
ignored workspace-local `.barracuda/selected-board` file. Scripts may pass an
explicit name as `cargo board select <board-name>`. Every active Board consumer
watches and reads the state file, so changing the selection invalidates the
relevant generated build output. A build with no selection stops with the
command needed to select one; it never guesses a Board from the Rust target.

`boards/selected` does not inspect the target OS or architecture. Platform
selection remains independent in `platforms/selected`; the Rust target chooses
the Platform, and Target composition rejects incompatible Board/Platform pairs.
Platform YAML remains beside its implementation at
`platforms/<name>/platform.yml`.

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
