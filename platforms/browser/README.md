# Browser Platform

The Browser Platform runs the complete selected Barracuda application in a
dedicated Web Worker. Its runtime and page bridge do not depend on a particular
System or Plugin graph.

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

- an in-page Embassy IP stack and Service Worker bridge to arbitrary System
  HTTP and WebSocket listeners;
- OPFS-backed native flash partitions;
- a worker-backed Embassy monotonic time driver;
- the Platform's explicitly selected plaintext TLS capability.

The Web Worker, WASI Preview 1 host, page-network bridge, System WASM, and boot
flash image are all files in the static website. There is no native gateway,
second process, WebSocket endpoint, or server-side runtime in the distributed
application. OPFS and Service Workers require a secure browser context, so a
deployed bundle must use HTTPS (localhost is also accepted by browsers for
development).

After startup, enter the port and path of any System HTTP service. The launcher
opens a virtual URL whose `_barracuda/<port>/` prefix is owned by the Platform;
the path presented to the System is unchanged. HTML loaded through that bridge
can use same-origin WebSockets on any path, including concurrent connections.

The page-local network covers System-owned HTTP and WebSocket endpoints without
knowing which Plugin registered them.
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
