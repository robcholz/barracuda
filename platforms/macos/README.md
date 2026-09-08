# macOS Platform

The macOS Platform runs the same `embassy_net::Stack` as device Platforms and
connects its layer-three driver to Barracuda's unprivileged user-space gateway.
It does not create UTUN interfaces, change PF rules, enable host forwarding, or
require `sudo`.

- guest stack: `10.0.2.15/24`
- virtual gateway: `10.0.2.2`
- virtual DNS: `10.0.2.3`
- packet transport: `ws://127.0.0.1:8787/v1/connect`

Start the gateway in one terminal:

```sh
cargo run -p barracuda-net-gateway --target "$(rustc -vV | sed -n 's/^host: //p')"
```

Then select the macOS Board and start Barracuda in another terminal:

```sh
cargo board select local-macos
cargo run
```

The gateway forwards device-originated TCP and UDP through ordinary host
sockets, translates DNS requests sent to `10.0.2.3`, and assigns each connected
device a loopback URL for its WebServer port. The macOS Platform logs that URL
when the session opens.

Platform, System, Core, and Plugin logs are written to standard error at
`info`. Select another level for a build with, for example,
`BARRACUDA_LOG_LEVEL=debug cargo run`.
