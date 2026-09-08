# Browser Platform

The Browser Platform runs the complete Barracuda System in a dedicated Web
Worker. It uses the same selected application and fixed Plugin graph as the
other Platforms, including Lua, LittleFS, the Captive Portal, and the shared
WebServer.

Select and use it through the normal Board workflow:

```sh
cargo board select browser
cargo build
cargo run
```

`cargo run` builds the host launcher, creates the selected System image,
generates the JavaScript bindings, starts an unprivileged loopback HTTP server
and network gateway, and opens the page in the default browser. Stop the
launcher with Ctrl-C. `cargo run -- --no-open` starts the same services without
opening a browser, which is useful for automated checks.

The browser owns only Platform mechanisms:

- an Embassy IP stack transported as versioned IP packets over a loopback
  WebSocket;
- OPFS-backed native flash partitions;
- a worker-backed Embassy monotonic time driver;
- the Platform's explicitly selected plaintext TLS capability.

The launcher exposes the device WebServer through an ephemeral loopback URL
shown on the page. Browser WebSocket origins are restricted to the launcher's
exact ephemeral origin. The gateway and forwarded device listener bind to
loopback by default and are not exposed to the LAN.

Lua's existing `vendored` feature and LittleFS's existing feature set are used
unchanged. Their low-level build scripts use checked-in `wasm32-wasip1` C
archives when Cargo targets WebAssembly, while the Rust System targets
`wasm32-unknown-unknown`. The launcher supplies the small WASI Preview 1 surface
retained by the linked image. Ordinary builds therefore do not require clang,
a WASI SDK, or manual C dependency setup; CI reproducibly rebuilds the archives
with pinned WASI SDK and Lua releases.
