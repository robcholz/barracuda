# Barracuda network gateway

`barracuda-platform-net-gateway` is an unprivileged, pure-Rust router for
Platforms that cannot attach an Embassy stack directly to a physical network
interface. Each WebSocket connection owns an isolated virtual-network session.

```sh
cargo run -p barracuda-platform-net-gateway --target "$(rustc -vV | sed -n 's/^host: //p')"
```

The default endpoint is `ws://127.0.0.1:8787/v1/connect`. A session translates
guest TCP and UDP flows to host sockets, maps the guest-visible `10.0.2.3:53`
resolver to `1.1.1.1:53`, and opens an ephemeral loopback listener forwarded to
guest `10.0.2.15:8787`. The assigned HTTP URL is returned to the device over the
same versioned protocol.

Native clients do not send a browser `Origin` header and work with the default
command. Browser clients require an exact origin allowlist entry, for example:

```sh
cargo run -p barracuda-platform-net-gateway --target "$(rustc -vV | sed -n 's/^host: //p')" -- \
  --allowed-origin http://localhost:3000
```

Use `--dns-server`, `--forward-address`, `--device-web-port`,
`--public-base-url`, and repeated `--allowed-origin` arguments to override those
defaults. Binding either the WebSocket listener or forwarded device ports
beyond loopback exposes the corresponding surface to that network and should
be an explicit deployment decision.
