# macOS Platform

The macOS Platform runs the same `embassy_net::Stack` as device Platforms and
connects its simulated layer-three driver to a real macOS UTUN interface.

- operating-system peer: `10.42.0.1/30`
- Embassy Net stack: `10.42.0.2/30`
- WebServer endpoint: `10.42.0.2:8787`

Build the application and the narrow privileged launcher, then run:

```sh
cargo build -p barracuda-cli -p barracuda-platform-macos --bins
sudo target/debug/barracuda-macos-network target/debug/barracuda
```

Platform, System, Core, and Plugin logs are written to standard error at
`info`. Select another level for a build with, for example,
`BARRACUDA_LOG_LEVEL=debug cargo build`.

The launcher creates UTUN, enables forwarding, installs a scoped PF NAT anchor,
and passes the UTUN descriptor to a Barracuda child dropped back to the invoking
user. It removes the anchor and restores forwarding when the child exits. The
application consumes the inherited descriptor through
`BARRACUDA_MACOS_UTUN_FD`; System and Plugins never receive a native socket
fallback.
