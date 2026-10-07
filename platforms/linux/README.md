# Linux Platform

The Linux Platform runs the same `embassy_net::Stack` as device Platforms and
connects its simulated layer-three driver to a real persistent Linux TUN.

Provision the TUN, forwarding, and NAT once per boot:

```sh
sudo sh platforms/linux/provision.sh barracuda0
```

The application then opens `barracuda0` without root privileges.

```sh
cargo board select local-linux
BARRACUDA_VIRTUAL_IO_ADDR=127.0.0.1:7878 cargo run
```

`BARRACUDA_VIRTUAL_IO_ADDR` is required: it is the loopback address where the
virtual peripherals manager listens. `local-linux` exposes the virtual pins
`vio-0` to `vio-7` and two virtual I2C controllers; the manager drives input
levels, attaches I2C devices, and injects faults. See
[`platforms/virtual-io`](../virtual-io/README.md).

This terminal owns the System and its logs. Start `cargo cli` in another
terminal to connect the external terminal Channel.

- operating-system peer: `10.42.0.1/30`
- Embassy Net stack: `10.42.0.2/30`
- WebServer endpoint: `10.42.0.2:8787`

Platform, System, Core, and Plugin logs are written to standard error at
`info`. Select another level for a build with, for example,
`BARRACUDA_LOG_LEVEL=debug cargo run`.
