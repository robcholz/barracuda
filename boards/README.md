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

The application build independently selects a Board with `BARRACUDA_BOARD`
and a Platform with `BARRACUDA_PLATFORM`. Platform YAML remains beside its
implementation at `platforms/<name>/platform.yml`.
