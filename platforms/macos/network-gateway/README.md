# macOS Platform network gateway

`barracuda-platform-macos-network-gateway` is the macOS Platform's
unprivileged, pure-Rust virtual router. Each connection owns an isolated
virtual-network session.

```sh
cargo run -p barracuda-platform-macos-network-gateway --target "$(rustc -vV | sed -n 's/^host: //p')"
```

The default endpoint is `ws://127.0.0.1:8787/v1/connect`. A session translates
guest TCP and UDP flows to host sockets, maps the guest-visible `10.0.2.3:53`
resolver to `1.1.1.1:53`, and opens an ephemeral loopback listener forwarded to
one configured guest TCP port (8787 by default). The assigned URL is returned
to the macOS guest over the same versioned protocol.

Native clients do not send a browser `Origin` header and work with the default
command. Browser clients require an exact origin allowlist entry, for example:

```sh
cargo run -p barracuda-platform-macos-network-gateway --target "$(rustc -vV | sed -n 's/^host: //p')" -- \
  --allowed-origin http://localhost:3000
```

Use `--dns-server`, `--forward-address`, `--forward-port`,
`--public-base-url`, and repeated `--allowed-origin` arguments to override those
defaults. Binding either the WebSocket listener or forwarded device ports
beyond loopback exposes the corresponding surface to that network and should
be an explicit deployment decision.
