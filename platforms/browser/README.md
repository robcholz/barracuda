# Browser Platform status

This directory contains an **experimental Browser Platform**. It is selectable
for `wasm32-unknown-unknown` and its Platform crate is checked in the normal CI
Platform matrix, but it is not yet an end-to-end Barracuda device.

The following pieces compile and have focused tests:

- the `barracuda_platform::Platform` implementation and worker entry;
- the dedicated-worker startup guard and Embassy executor;
- the versioned WebSocket packet framing;
- construction of an Embassy driver-channel device and stack;
- OPFS-backed native partitions for System, resources, and the Plugin database.

The end-to-end browser device described by the Platform manifest is not yet
available. In particular:

- the repository's fixed System Plugin graph includes the vendored Lua runtime,
  whose C implementation does not support `wasm32-unknown-unknown`, so the
  complete selected application is not built for this Board yet;
- the Browser Platform currently exposes explicit plaintext HTTP rather than a
  verified browser-compatible TLS capability;
- the gateway's packet session does not yet run libslirp or install a real
  WebServer port forward;
- macOS still uses its existing privileged UTUN launcher.

Consequently, compiling this Platform demonstrates its target composition and
persistent storage boundary, but not Internet access, complete System startup,
or macOS gateway compatibility. Do not deploy it until those paths have
end-to-end coverage in a real browser and against a libslirp-enabled gateway.
