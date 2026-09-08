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

Building requires WASI SDK 33 with `WASI_SDK_PATH` set to its extracted root.
That SDK is only the C compiler/sysroot for the bundled Lua and LittleFS
sources; it is not part of the runtime or the distributed page.

`cargo run` creates `target/barracuda-browser`, a self-contained static website,
serves it on an unprivileged loopback port for development, and opens the page.
Stop the development server with Ctrl-C. `cargo run -- --no-open` exports and
serves the same bundle without opening a browser. The exported directory can be
deployed at the origin root of an ordinary HTTPS static host; the host process
is not part of the distributed Browser application.

The browser owns only Platform mechanisms:

- an in-page Embassy IP stack and Service Worker bridges for `/portal/*` HTTP
  plus the Portal's WebSocket, both terminating at the System's unchanged
  WebServer;
- OPFS-backed native flash partitions;
- a worker-backed Embassy monotonic time driver;
- the Platform's explicitly selected plaintext TLS capability.

The Web Worker, WASI Preview 1 host, page-network bridge, System WASM, and boot
flash image are all files in the static website. There is no native gateway,
second process, WebSocket endpoint, or server-side runtime in the distributed
application. OPFS and Service Workers require a secure browser context, so a
deployed bundle must use HTTPS (localhost is also accepted by browsers for
development).

The page-local network covers the System-owned HTTP and WebSocket endpoints.
Browsers do not expose arbitrary raw internet TCP or UDP sockets, so the
Platform does not pretend to provide those routes or require a hidden server
proxy; Plugins that contact external services still remain selected, but such
calls cannot succeed through the page-local stack.

Lua's existing `vendored` feature and LittleFS's existing feature set are used
unchanged. For `wasm32-wasip1`, their low-level build scripts compile the
bundled C sources with the WASI SDK selected by `WASI_SDK_PATH`. Rust, Lua, and
LittleFS consequently share one target ABI. No precompiled C archive is stored
in the repository or included separately in the website; C objects are linked
into `barracuda_system.wasm` by Cargo. The SDK is a build dependency only and is
not required by people opening the resulting page. CI installs the pinned WASI
SDK 33 release before running the normal Cargo commands.
