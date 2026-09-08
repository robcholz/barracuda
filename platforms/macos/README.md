# macOS Platform

The macOS Platform runs the same `embassy_net::Stack` as device Platforms and
connects its simulated layer-three driver to a real macOS UTUN interface.

- operating-system peer: `10.42.0.1/30`
- Embassy Net stack: `10.42.0.2/30`
- WebServer endpoint: `10.42.0.2:8787`

Select the macOS Board and run normally:

```sh
cargo board select local-macos
cargo run
```

This terminal owns the System and its logs. In another terminal, connect the
external terminal Channel with `cargo cli`.

The Platform manifest installs a Cargo runner for this selection. `cargo run`
uses that runner to compile `barracuda-macos-network` for the host, launch the
standalone `barracuda-system` application through `sudo`, and pass it the
inherited UTUN descriptor. A plain `cargo build` compiles the selected
application without starting networking.

Platform, System, Core, and Plugin logs are written to standard error at
`info`. Select another level for a build with, for example,
`BARRACUDA_LOG_LEVEL=debug cargo run`.

The launcher creates UTUN, enables forwarding, installs a scoped PF NAT anchor,
and passes the UTUN descriptor to a Barracuda child dropped back to the invoking
user. It removes the anchor and restores forwarding when the child exits. The
application consumes the inherited descriptor through
`BARRACUDA_MACOS_UTUN_FD`; System and Plugins never receive a native socket
fallback.
