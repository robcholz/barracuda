# macOS Platform

The macOS Platform runs the same `embassy_net::Stack` as device Platforms and
connects its layer-three driver to Barracuda's unprivileged user-space gateway.
It does not create UTUN interfaces, change PF rules, enable host forwarding, or
require `sudo`.

- guest stack: `10.0.2.15/24`
- virtual gateway: `10.0.2.2`
- virtual DNS: `10.0.2.3`
- packet transport: `ws://127.0.0.1:8787/v1/connect`

Select the macOS Board and use the normal workflow:

```sh
cargo board select local-macos
cargo build
cargo run
```

`cargo run` builds the Platform launcher, deploys the selected System image,
starts the loopback network gateway, and starts the System application. The
launcher owns both processes, so Ctrl-C or SIGTERM stops the complete run. No
second terminal or manual gateway command is required.

The gateway forwards device-originated TCP and UDP through ordinary host
sockets, translates DNS requests sent to `10.0.2.3`, and assigns a loopback
URL to the configured guest TCP port. The macOS Platform logs that URL when
the session opens and publishes it to `.barracuda/address`. `cargo cli` and the
model configuration helper discover the active address from that file, so an
ephemeral loopback port does not need to be copied into `.env.local`.

Platform, System, Core, and Plugin logs are written to standard error at
`info`. Select another level for a build with, for example,
`BARRACUDA_LOG_LEVEL=debug cargo run`.
