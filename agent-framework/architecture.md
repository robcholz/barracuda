# Architecture

The production framework is `#![no_std]` with `alloc`. It is executor-neutral:
it never creates an OS thread and never selects a concrete async runtime.

## Runtime shape

`AgentSystem::new` returns two values:

- `AgentSystem`: the clone-free application control surface. Session operations
  are async and communicate with the service through no_std `async-channel`.
- `AgentService`: one long-running future. The application spawns this future on
  Embassy or another executor.

`AgentService` is intentionally `!Send`: its state and internal yield streams
use single-executor `Rc`/`RefCell` ownership instead of synchronization. Embassy
can spawn it with the current executor's `Spawner`; it cannot be submitted
directly through `SendSpawner` or moved to an interrupt/other-thread executor.
Keep the service and its `AgentSystem` control surface on one executor.

The application supplies `ClawFs` and an `embedded-nal-async::TcpConnect + Dns`
network stack. `claw-api` drives reqwless directly, while `claw-net` contains
only platform/test adapters; the core has no chip-specific dependency. Timeouts and backoff
use `embassy-time` directly. The final application supplies Embassy's global
time driver through its HAL; there is no framework timer trait or timer generic.
Host binaries enable Embassy's `std` driver while retaining Tokio only for the
executor and TCP/DNS implementation.

`ClawApi` owns its reqwless client, persistent `HttpResource`, and reusable HTTP
buffers exclusively. `ClawApiFactory` controls construction at the application
boundary; it is not a request-time pool. Portable firmware can select
`embedded-tls` with certificate verification, while a platform can select the
mbedTLS backend. Only the TCP/DNS/TLS HAL changes—the reqwless HTTP path does
not fork between host and device.

## Crates

- `claw-interface`: filesystem platform traits.
- `claw-net`: TCP/DNS platform and deterministic test adapters.
- `claw-utils`: no_std identifiers, task-pool, and text utilities.
- `claw-api`: executor-neutral reqwless LLM clients and TLS ownership.
- `claw-permission`, `claw-sandbox`, `claw-tool`: policy and tool runtime.
- `claw-context`, `claw-memory`, `claw-skill`, `claw-persistence`: durable
  context and storage subsystems.
- `claw-core`: session actors, agent iteration loop, and `AgentService`.
- `claw-agent`: assembled public API.
- `claw-cli` and `claw-log`: host-only tools; they may use `std`.

The old C bridge crates have been removed. Firmware integration is now direct
Rust through application-owned HAL adapters and an Embassy spawner.
