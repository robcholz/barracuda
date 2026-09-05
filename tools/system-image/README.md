# System Image Tool

This host-only crate builds and flashes the repository's top-level `image/`
source tree for the currently selected Board's writable `system` region.

```sh
cargo board select
cargo system-image build
cargo system-image flash
```

Both operations read `.barracuda/selected-board`; neither accepts a Board or
Platform argument. `build` reads the Board's native physical layout and writes
`target/barracuda-system.img`. `flash` requires that complete image to already
exist and writes it only to the selected Board's native `system` partition.

The shared Platform resolver checks the selected Board's chip and declared
toolchain target, then chooses the internal Platform flasher. The runtime
Platform build uses the same target-to-Platform registry, so system-image does
not maintain its own chip mapping or Platform selection state.

- macOS and Linux update the file-backed flash configured by that Platform's
  `platform.yml`, preserving all other regions;
- ESP Platforms call `espflash write-bin` at the partition-table offset;
- STM32 Platforms call `probe-rs download` with the linker-defined absolute
  base address.

Adding another Board that uses an existing Platform requires no new CLI
configuration. A new Platform family adds one internal flasher and one native
layout resolver while the CLI remains Board-selected.

Paths below the source directory are preserved relative to the mounted root;
for example, `image/system/workflows.json` becomes
`/system/workflows.json`.
