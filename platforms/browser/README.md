# Browser Platform status

This directory is an **experimental transport prototype**, not a runnable
Barracuda Platform yet.

The following pieces compile and have focused tests:

- the `wasm32-unknown-unknown` crate;
- the dedicated-worker startup guard;
- the versioned WebSocket packet framing;
- construction of an Embassy driver-channel device and stack;
- standalone OPFS JavaScript helpers.

The end-to-end browser device described by the Platform manifest is not yet
available. In particular:

- `BrowserPlatform` does not implement `barracuda_platform::Platform`;
- `start` does not create an Embassy executor, initialize partitions, create
  the stack, or invoke the Barracuda System application;
- the downloaded System image is only checked for emptiness and is not mounted
  or persisted;
- the OPFS helpers are not connected to a Rust `Partitions` implementation;
- browser timer support has not been wired into the Platform;
- the gateway's packet session does not yet run libslirp or install a real
  WebServer port forward;
- macOS still uses its existing privileged UTUN launcher.

Consequently, compiling these crates does not demonstrate Internet access,
System startup, persistence, or macOS compatibility. Do not deploy or select
this Platform until those items have end-to-end coverage in a real browser and
against a libslirp-enabled gateway.
