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

The application supplies `ClawFs`, `ClawHttp`/`StreamingHttp`, and `ClawTimer`
implementations. HALs belong in adapter crates outside the framework; the core
has no chip-specific dependency.

## Crates

- `claw-interface`: platform traits and shared boundary types.
- `claw-utils`: no_std identifiers, task-pool, and text utilities.
- `claw-api`: executor-neutral streaming LLM clients.
- `claw-permission`, `claw-sandbox`, `claw-tool`: policy and tool runtime.
- `claw-context`, `claw-memory`, `claw-skill`, `claw-persistence`: durable
  context and storage subsystems.
- `claw-core`: session actors, agent iteration loop, and `AgentService`.
- `claw-agent`: assembled public API.
- `claw-cli` and `claw-log`: host-only tools; they may use `std`.

The old C bridge crates have been removed. Firmware integration is now direct
Rust through application-owned HAL adapters and an Embassy spawner.
