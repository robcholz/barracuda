# Async Execution Ownership

This document defines ownership of long-lived futures across Platform, HAL,
System, and Plugins. Current code that contradicts these boundaries is
migration work, not precedent.

## Core boundary

The subsystem that defines a task's behavior also owns its lifecycle, capacity,
failure policy, and cancellation. Long-lived work runs as an Embassy task or
task pool started by that owner.

```text
Embassy executor
+-- Platform-owned tasks
|   `-- network runner
+-- HAL-owned tasks
|   `-- peripheral Driver runners
+-- System-owned tasks
`-- Plugin-owned tasks
    +-- Workflow matching and execution
    +-- WebServer accept and connection workers
    +-- Scheduler due-occurrence loop
    `-- other Plugin services
```

A typed capability determines how another Plugin calls a service. A Workflow
Action or Event determines how Workflow integrates with that service. Neither
contract transfers execution ownership away from the subsystem that implements
the behavior.

## Ownership table

| Work | Owner | Execution |
| --- | --- | --- |
| Platform network runner | Platform | Embassy task |
| Peripheral Driver runner | HAL composition | Embassy task |
| WebServer accept loop | WebServer Plugin | Embassy task |
| WebServer connection workers | WebServer Plugin | fixed Embassy task pool |
| Network time synchronization | Time Plugin | cancellable Embassy task |
| Scheduler due-occurrence loop | Scheduler Plugin | cancellable Embassy task |
| Workflow matching and execution | Workflow Plugin | cancellable Embassy task |
| Continuous work behind a typed capability | Capability provider | owner-managed Embassy task |

Platform and HAL start only the runners for resources they construct. System
starts System-owned work and installs the Embassy spawner used by Plugin
startup. Each Plugin starts its own tasks after the complete Plugin capability
and registration graph exists.

## Plugin startup and cancellation

`Plugin::register` synchronously constructs capabilities and installs retained
registrations. `Plugin::start` synchronously spawns long-lived work through
`PluginStartContext::task_spawner`.

Each permanent Plugin task receives one manager-owned `PluginTaskToken`. The
task races its owner loop with `PluginTaskToken::cancelled` and remains
cancellation-safe at every await point. Startup rollback, Plugin unload, and
Plugin Manager teardown signal all tokens and wait for their task-side tokens
to drop before releasing capabilities and retained resources. This makes an
immediate unload and reload safe for Embassy's fixed task pools.

Long-lived fixed work uses `#[embassy_executor::task]` and, when needed,
statically allocated task pools. Task and worker capacity belongs to the owning
subsystem's configuration. A spawn failure is a startup failure of that owner
and propagates through System startup.

## Invariants

- The layer that defines continuous behavior owns its task and failure policy.
- Platform and HAL runners remain with the resources they drive.
- External protocol listeners and connection workers belong to their protocol
  Plugin.
- A task consumes handles or typed capabilities; business behavior does not
  move into Platform merely to obtain a `Spawner`.
- Plugin registration finishes before Plugin tasks accept external work.
- Permanent Plugin tasks stop cooperatively on unload, startup rollback, and
  Plugin Manager teardown.

## Review checklist

Before adding a long-lived future, verify:

1. Which subsystem defines the work's behavior, capacity, and failure policy?
2. Which Platform, HAL, System, or Plugin lifecycle should start and stop it?
3. Can it use a statically allocated Embassy task or task pool?
4. Does startup guarantee that dependencies and retained registrations exist
   before the task begins accepting work?
5. Does cancellation release every leased worker, queue, socket, and
   registration required for immediate reload?
