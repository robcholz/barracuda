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
BARRACUDA_VIRTUAL_IO_ADDR=127.0.0.1:7878 cargo run
```

`BARRACUDA_VIRTUAL_IO_ADDR` is required: it is the loopback address where the
virtual peripherals manager listens (any free port other than the gateway's
8787). `local-macos` exposes the virtual pins `vio-0` to `vio-7` and two
virtual I2C controllers. See [`platforms/virtual-io`](../virtual-io/README.md).

`cargo run` builds the Platform launcher, deploys the selected System image,
starts the loopback network gateway, and starts the System application. The
launcher owns both processes, so Ctrl-C or SIGTERM stops the complete run. No
second terminal or manual gateway command is required.

The gateway forwards device-originated TCP and UDP through ordinary host
sockets, translates DNS requests sent to `10.0.2.3`, and assigns a loopback
URL to the configured guest TCP port. The macOS Platform logs that URL when
the session opens and publishes it to `.barracuda/address`. The CLI discovers
the active address from that file, so an ephemeral loopback port does not need
to be copied into local configuration. Its Local/Remote selector shows the
discovered Local address before it connects. `cargo cli configure` uses the
same selector and opens the active System's Plugin configuration portal.

Platform, System, Core, and Plugin logs are written to standard error at
`info`. Select another level for a build with, for example,
`BARRACUDA_LOG_LEVEL=debug cargo run`.
