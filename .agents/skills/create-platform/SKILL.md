---
name: create-platform
description: Add or update a Barracuda Platform — platform.yml, the Platform trait, the entry macro, vendor HAL adaptation, native layout projection — with its CI coverage and English and Chinese docs pages.
---

# Create Platform

[Platform architecture](../../../docs/architecture/platform-architecture.mdx)
is the contract: what a Platform owns, the exact `PlatformResources` shape,
and the review checklist at its end. Read it first, then read the Platform
closest to the new one under `platforms/` and its page under
`docs/platforms/`.

A Platform is one execution platform: one chip (`esp32c6`, `stm32`) or one
hosted environment (`linux`, `macos`). It supplies the IP stack, Wi-Fi
control, entropy, and partitions, and adapts its vendor HAL to `embedded-hal`
contracts. Concrete Board hardware is never a Platform concern.

## 1. Decide the scope

- Name the Platform after its chip or environment. Each chip is its own
  Platform, even inside one vendor ecosystem: chips that share one declare
  the same `info.family` and share source with `include!` from a sibling
  Platform (the ESP chips include `platforms/esp32/src/`), but are never
  merged into one family Platform.
- Adding a Platform changes no Platform API, no Board, no System code, and
  no Plugin. A missing upstream contract becomes a narrow binding beside the
  peripheral API, not a Platform field.

## 2. Lay out the crate

```text
platforms/<name>/
├── platform.yml        # identity, toolchain, HAL inventory, selection, image
├── Cargo.toml          # package barracuda-platform-<name>
├── build.rs            # projects the selected Board's native layout
├── src/
│   ├── lib.rs          # the Platform type, partitions, errors
│   ├── application.rs  # the platform_entry! macro
│   ├── hal.rs          # vendor HAL adaptation
│   └── entropy.rs
└── tests/
```

Copy the `[lints]` tables of an existing Platform.

## 3. Write `platform.yml`

Start from the closest Platform and keep its key order:

- `name`, `info.family`, `info.environment` (`bare-metal` or `hosted`),
  `package`, `crate`, and `type` (the Rust type implementing `Platform`);
- `build`: `rustup-toolchain`, optional `environment-file`, the
  `missing-toolchain-prompt` that `cargo board select` prints, and an
  optional `linker-script`, a top linker script the Platform's build script
  puts on the link path in place of its runtime crate's (the ESP32 and
  ESP32-P4 widen esp-hal's flash window this way);
- `hal`: `bindings`, the binding categories Boards may use in
  `peripherals.io`, and the runtime controller groups (`runtime-i2c-controllers`,
  `runtime-adc-controllers`, ...) exposed I/O draws from; host Platforms
  set `peripheral-models: true`;
- `selection`: the `board-chips` Boards name in `hardware.chip`, and the
  Rust `targets` (`triple`, or `os` for hosted);
- `system-image` layout and flash drivers, and the `application` launcher
  `cargo run` uses.

## 4. Implement

- **`Platform`.** Implement `barracuda_platform::Platform` with `Bindings`
  (only the chip resources Platform mechanisms consume), `Wifi`, `Entropy`,
  `Partitions`, and `Error`. `initialize` builds exactly
  `PlatformResources { ip_stack, wifi, entropy, partitions }` and spawns
  only the Platform's own tasks.
- **Wi-Fi.** A Platform without a radio supplies an explicitly unsupported
  implementation.
- **Entropy.** The OS generator on hosted Platforms, the hardware TRNG on
  devices; `UnavailableEntropy` when there is none, never a weaker
  generator.
- **Partitions.** One generic collection projected from the Board's native
  layout, each entry carrying access and filesystem metadata (`raw`,
  `fatfs`, `littlefs`). Business roles are never fields.
- **Entry.** `application.rs` exports `platform_entry!`, which
  `composition/selected` calls. It owns the target ABI, executor bootstrap,
  and termination, and acquires the chip singleton exactly once. It parses
  no YAML, wires no peripheral, and does not depend on System.
- **HAL.** `hal.rs` converts the vendor's move-only tokens into
  `embedded-hal` and `embedded-hal-async` values for every category in
  `hal.bindings`, and builds the runtime controller pools and pin mux.
  Peripheral implementations never import the vendor HAL.
- **`build.rs`.** Read the selected Board with `barracuda_board_config`. The
  crate builds for every selection, so when the Board's chip is not this
  Platform's, emit an empty inactive projection; otherwise validate the
  native layout and generate its regions. Never pick a Board from
  `target_os` or `target_arch`.
- Gate chip code behind the target's `cfg`, and keep the layout projection
  outside it so host tests run.

## 5. Register

- Run `cargo platform sync`, which writes the workspace dependency block in
  the root `Cargo.toml`.
- Add at least one Board with `create-board`; a Platform without a Board
  cannot be selected or built.
- In `.github/workflows/ci.yml`: add the name to the "Check CI covers every
  Platform" list, add `--exclude barracuda-platform-<name>` to both host
  jobs' `PLATFORM_EXCLUDES`, add a `platforms` matrix entry with a
  representative Board and toolchain, and extend the Board-grouping toolchain
  mapping for a new target prefix.
- `git grep` an existing Platform name outside its directory (for example
  `esp32c6`) and extend every list that enumerates all Platforms, such as
  `tools/system-image/tests/platform_boundary.rs` and vendored per-chip
  features.

## 6. Verify

```bash
cargo platform sync --check
cargo board sync --check
cargo board select <board-id>
cargo check --package barracuda-platform-<name>
cargo clippy --package barracuda-platform-<name> -- -D clippy::disallowed_types
cargo test --target host-tuple --package barracuda-platform-<name>
cargo build --package barracuda-platform-<name>
```

Then select `local-linux` again for host work. Leave the full matrix to CI.

## 7. Document

There is one page per family, `docs/platforms/<family>.mdx`, with a
`zh/` copy.

- A new chip in an existing family adds a row to the page's table (chip and
  Platform name, Rust target, toolchain, Board count), its install
  instructions, and what its HAL drives.
- A new family adds a page in the same form: frontmatter `title`
  ("\<Family\> Platform"), `sidebarTitle`, and a `description` starting
  "Build, flash, and run Barracuda on"; a first screen with what runs and the
  chip table; then `## Install the toolchain`, `## Build, flash, and run`,
  `## Hardware the Platform drives`, and `## Boards`.

In the same change:

- write the Chinese copy under `docs/zh/platforms/`, following
  [Writing docs](../../../docs/contributing/docs.mdx#languages);
- add a new page to the Platforms group of both navigations in
  `docs/docs.json`;
- update the toolchain steps in `docs/get-started/installation.mdx`, the
  `platforms/` tree in `docs/architecture/platform-architecture.mdx`, and
  their `zh/` copies when they change;
- add the chip to `CHIPS` in `website/src/content/boards.ts`.

Check with `bun test tools/web` and, from `docs/`, `npx mint validate` and
`npx mint broken-links`.
