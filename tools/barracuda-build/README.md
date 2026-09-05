# Barracuda Build Driver

This host tool is the default member behind ordinary `cargo run`. It reads the
persisted Board selection, discovers its Platform from the self-described
`platforms/*/platform.yml` catalog, and projects the source tree into
`target/barracuda-workspace`.

Only the generated copies of `platforms/selected/Cargo.toml` and
`boards/selected/Cargo.toml` receive concrete dependencies. Their dependency
names are stable aliases; the source manifests contain no Platform or Board
registry.

```sh
cargo board select
cargo run
cargo barracuda build
```

Adding a Platform and Board requires only:

- `platforms/<platform>/`, including its `platform.yml` and implementation;
- `boards/configs/<board>/`, including `board.yml` and native layout files.

If the Board exposes hardware, `board.yml` points its `board-hal.path` to the
adapter crate, which may be stored below the owning Platform directory.
