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
cargo run
```

- operating-system peer: `10.42.0.1/30`
- Embassy Net stack: `10.42.0.2/30`
- WebServer endpoint: `10.42.0.2:8787`

Platform, System, Core, and Plugin logs are written to standard error at
`info`. Select another level for a build with, for example,
`BARRACUDA_LOG_LEVEL=debug cargo run`.
