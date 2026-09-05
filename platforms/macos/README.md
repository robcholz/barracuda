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

The Platform manifest makes the build driver compile
`barracuda-macos-network` automatically and launch it through `sudo`; the
application receives the inherited UTUN descriptor. Use
`cargo barracuda build` to build both binaries without starting them.

Platform, System, Core, and Plugin logs are written to standard error at
`info`. Select another level for a build with, for example,
`BARRACUDA_LOG_LEVEL=debug cargo run`.

The launcher creates UTUN, enables forwarding, installs a scoped PF NAT anchor,
and passes the UTUN descriptor to a Barracuda child dropped back to the invoking
user. It removes the anchor and restores forwarding when the child exits. The
application consumes the inherited descriptor through
`BARRACUDA_MACOS_UTUN_FD`; System and Plugins never receive a native socket
fallback.
