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

The shared Platform resolver discovers `platforms/*/platform.yml`, checks the
selected Board's chip and declared toolchain target, and reads that Platform's
`system-image` drivers. The runtime build and system-image tool therefore share
the same filesystem catalog without a central chip or Platform registry.

Layout drivers support file-region YAML, ESP-IDF partition CSV, linker
`MEMORY`, or a Platform-local command returning the standard region response.
Flash drivers either update a configured file-backed flash or execute a
shell-free command template with Board chip, offset, size, layout, and image
placeholders.

Adding another Board that uses an existing Platform requires no new CLI
configuration. A new Platform declares its selection and image behavior in its
own `platform.yml`; custom behavior can ship as a command inside that same
Platform directory.

Paths below the source directory are preserved relative to the mounted root;
for example, `image/system/workflows.json` becomes
`/system/workflows.json`.
