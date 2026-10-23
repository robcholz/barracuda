---
name: create-board
description: Add or update a Barracuda Board bundle — board.yml, native flash layout, peripherals, exposed I/O, profile and Plugin overrides — with its English and Chinese docs pages and website entry.
---

# Create Board

[Platform architecture](../../../docs/architecture/platform-architecture.mdx)
(the Board sections) is the contract. [Boards](../../../docs/boards/overview.mdx),
[Peripheral composition](../../../docs/boards/peripherals.mdx),
[Exposed I/O](../../../docs/boards/exposed-io.mdx), and
[Board profiles](../../../docs/boards/profiles.mdx) describe the current
schema. Read them first, then read two existing bundles under
`boards/configs/` on the same chip.

A Board is data. A new Board that reuses existing peripheral implementations
adds YAML, its native layout, and its pages, and no Rust.

## 1. Pin down the product

- One directory per concrete product: `boards/configs/<board-id>/`, where
  `<board-id>` is lowercase and hyphenated, vendor first
  (`m5stack-atom-matrix`), or the vendor's kit name for Espressif kits
  (`esp32c6-devkitc-1`). Two products on the same module are two bundles,
  never an alias.
- Find the Platform whose `selection.board-chips` in
  `platforms/*/platform.yml` lists the chip. If none does, add the Platform
  first with `create-platform`.
- Take every pin, bus, address, polarity, and geometry from the vendor
  schematic or datasheet. Leave a device out until its wiring is confirmed;
  the Board page says what is still missing.

## 2. Write `board.yml`

```yaml
name: <board-id>                  # equals the directory name
profile: screen                   # omit for headless
hardware:
  chip: esp32s3                   # a chip the Platform's board-chips lists
toolchain:
  target: xtensa-esp32s3-none-elf # omitted on host Boards
native-layout:
  artifact: partitions.csv
peripherals:
  io: { ... }                     # private buses and pins, named by the Board
  devices: { ... }                # one entry per attached device
exposed-io:
  pins:
    port-a-sda: { pin: GPIO2 }
plugins:
  imessage-gateway:
    receive-slots: 2
```

- `board.yml` holds hardware facts, implementation IDs, and chip-native
  names. It never names a Platform, a Rust type, a crate, or a Cargo feature.
- `peripherals.io` declares private resources by binding category
  (`i2c-device`, `spi-device`, `quad-spi-device`, `spi-output`, ...); the
  selected Platform must list each category in its `hal.bindings`.
- Each `peripherals.devices` entry names an `implementation` that exists under
  `peripherals/impl/<kind>/<implementation-id>/peripheral.yml` and fills that
  manifest's bindings and parameters. A Board may have several devices of
  any kind except `removable-storage`; with several devices of a kind, mark
  exactly one `primary: true`, the product's main one. A device that fails
  to initialize at boot is logged and the Board starts without it.
- `exposed-io.pins` names only the pins applications may claim at runtime,
  each once, by a stable name printed on the product (`port-a-sda`,
  `expansion-1`). A pin used by a peripheral is never also exposed.
  Controller inventory belongs to the Platform manifest, not the Board.
- `profile` and `plugins.<id>` are the only places a Board names a Plugin:
  `enabled`, settings declared in that Plugin's `[config.*]`, and
  `peripherals` wiring only where slot defaults do not resolve.
- Comment non-obvious values with their source, such as an I2C address
  derived from strap pins.

## 3. Add the native layout

Copy the layout of a Board on the same chip and flash size:
`partitions.csv` (ESP-IDF) on ESP chips, `memory.x` on STM32,
`file-layout.yml` on host Boards. The native file is the only layout source;
never add a converter or a second table.

An ESP table has `ota_0` alone on 4 MiB and 8 MiB of flash and `ota_0` plus
`ota_1` on 16 MiB, never a `factory` app
([ESP flash layout](../../../docs/boards/overview.mdx#esp-flash-layout)).
Then pick a profile whose firmware fits the slot
([Fitting the flash](../../../docs/boards/profiles.mdx#fitting-the-flash)).

## 4. Add missing hardware support

When a device has no implementation yet, add it in its own layer:

1. a chip driver under `drivers/chips/<chip>/`, when no upstream crate covers
   the chip, depending only on `embedded-hal` contracts
   ([Chip drivers](../../../docs/boards/drivers.mdx));
2. an implementation under `peripherals/impl/<kind>/<implementation-id>/`
   with its `peripheral.yml`;
3. for a new semantic kind, its API in `peripherals/api` first.

Helper chips (expanders, bus switches) stay private to the implementation
that uses them. A new binding category is a Platform change; see
`create-platform`.

## 5. Sync and verify

Run `cargo board sync` and commit its output, then:

```bash
cargo board sync --check
cargo plugin sync --check
cargo board select <board-id>
cargo check
cargo clippy
cargo build --release
```

Run the `local-linux` selection again before host tests. Use focused checks
only; the full Board matrix runs in CI.

## 6. Document

Every Board has `docs/boards/supported/<board-id>.mdx`. Copy the page of a
Board on the same chip and keep its form:

- frontmatter `title` (vendor and product name), `sidebarTitle` (product
  name), and `description` "Build, flash, and run Barracuda on the …, an
  \<chip\> Board.";
- one sentence naming the Board, then the bullets in this order: Board ID,
  Vendor, Chip, Platform (linked), Rust target, Profile (linked), Flash layout;
- `## Build and run` with `cargo board select <board-id>`, `cargo run`,
  `cargo image deploy`, and a link to the Platform page for the toolchain;
- `## Peripherals`: a `Device | What it is | Lua package` table, or the
  standard "declares no peripherals yet" paragraph;
- `## Exposed pins` with a name-to-pin table and a short `gpio` example when
  the Board exposes pins, and `## Plugin settings` when it sets any.

Then, in the same change:

- add the Chinese copy at `docs/zh/boards/supported/<board-id>.mdx`, following
  [Writing docs](../../../docs/contributing/docs.mdx#languages): same code,
  IDs, and paths, `/zh` links without anchors;
- add both pages to `docs/docs.json` under Boards, in the vendor's group of
  each navigation, creating the group for a new vendor;
- on the Platform page under `docs/platforms/` and its `zh/` copy, update the
  Board count per chip, or the `## Boards` list where the page lists every
  Board;
- add the Board to `BOARDS` in `website/src/content/boards.ts`, and its chip
  to `CHIPS` when new;
- update every stated Board count: `git grep` the old number in `website/`
  and `docs/`.

Check with `bun test tools/web` and, from `docs/`, `npx mint validate` and
`npx mint broken-links`.
