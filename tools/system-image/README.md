# System Image Tool

This host-only crate builds the repository's top-level `image/` source tree
into a raw LittleFS image for the currently selected Board's writable `system`
region.

```sh
cargo board select
cargo system-image build
```

The command reads `.barracuda/selected-board` and the Board's native physical
layout. It writes `target/barracuda-system.img` and reports the native offset
where flash tooling must place the complete artifact.

Paths below the source directory are preserved relative to the mounted root;
for example, `image/system/workflows.json` becomes
`/system/workflows.json`.
