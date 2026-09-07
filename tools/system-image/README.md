# System Image Tool

This host-only crate builds a FATFS or LittleFS image containing enabled Plugin
resources and flashes it into the selected Board's read-only-at-runtime
`resources` region.

```sh
cargo board select
cargo system-image build
cargo system-image flash
```

Both operations read `.barracuda/selected-board`; neither accepts a Board or
Platform argument. `build` reads the Board's native physical layout and writes
`target/barracuda-system.img`. `flash` requires that complete image to already
exist and writes it only to the selected Board's native `resources` region.
The Board-native `resources` partition entry chooses `fatfs` or `littlefs`:
file-region layouts use that region's `filesystem` field, ESP layouts use its
partition subtype, and linker layouts use its inline `filesystem` annotation.
The `resources` region must be read-only at runtime.

The shared Platform resolver discovers `platforms/*/platform.yml`, checks the
selected Board's chip and declared toolchain target, and reads that Platform's
`system-image` drivers. The runtime build and system-image tool therefore share
the same Platform catalog without a central chip or Platform registry.

Layout drivers support file-region YAML, ESP-IDF partition CSV, linker
`MEMORY`, or a Platform-local command returning the standard region response.
Flash drivers either update a configured file-backed flash or execute a
shell-free command template with Board chip, offset, size, layout, and image
placeholders.

Adding another Board that uses an existing Platform requires no new CLI
configuration. A new Platform declares its selection and image behavior in its
own `platform.yml`; custom behavior can ship as a command inside that same
Platform directory.

Files below `plugins/<directory>/filesystem/resources/` are collected using
the stable ID from `plugin.toml` and become `/plugins/<id>/` in the selected
filesystem image. System mounts the image at `/resources`, and Plugin Manager
maps global `/resources/plugins/<id>/` to that Plugin's logical `/resources/`.
Files below `plugins/<directory>/filesystem/workspace/resources/` merge into
the image's `/workspace/` tree, which Plugin Manager exposes as the shared
logical `/workspace/resources/` mount. Conflicting shared paths fail the build
instead of making Plugin discovery order observable. Disabled Plugins are not
bundled, and mutable `/data`, `/cache`, and `/media` trees can never be
prebuilt.
