# Async Execution Ownership

This document defines which long-lived futures belong to Event Router and
which run as owner-managed Embassy tasks. It is authoritative for async
execution ownership across Platform, HAL, System, and Plugins. Current code
that contradicts this boundary is migration work, not precedent.

## Core boundary

Event Router executes Event Router contracts. It is not a general-purpose
executor or a registry for arbitrary background services.

An Event Router `Component` owns registrations and runtime work that directly
participate in Event Router behavior. Its `run` future may advance work such
as:

- emitting Events into Event Router;
- consuming routed Events or advancing Workflow execution;
- serving an Event Router RPC or Agent contract whose state must be driven by
  that Component.

A long-lived future that does not directly advance one of those contracts runs
as an Embassy task owned by the subsystem that needs it. Socket accept loops,
HTTP connection workers, Platform network runners, peripheral Driver runners,
and unrelated periodic service loops do not become Event Router Components.

~~~text
Embassy executor
+-- Platform-owned tasks
|   `-- network runner
+-- HAL-owned tasks
|   `-- peripheral Driver runners
+-- System-owned tasks
+-- Plugin-owned tasks
|   `-- WebServer accept and connection workers
`-- Event Router
    +-- Event producers and consumers
    +-- Workflow runtime
    `-- Event Router RPC and Agent contract Components
~~~

Event Router remains a cooperative async runtime internally, but that does not
make it the owner of every cooperative future in the application.

## Component membership test

Put a runtime loop in `Component::run` only when polling that loop directly
progresses an Event Router-visible contract.

Use an owner-managed Embassy task when any of these statements is true:

- the loop would still be required if Event Router were removed;
- its primary input is a socket, device, timer, filesystem, or Driver rather
  than an Event Router Event or call;
- its output is an external protocol response or capability state rather than
  an Event Router Event, Workflow transition, or RPC result;
- placing it in Event Router merely provides somewhere to poll the future.

A Component that only registers handlers may have no independent runtime loop.
Unrelated service work must not be added to give that Component something to
poll.

## Ownership table

| Work | Owner | Execution |
| --- | --- | --- |
| Platform network runner | Platform | Embassy task |
| Peripheral Driver runner | HAL composition | Embassy task |
| WebServer accept loop | WebServer Plugin | Embassy task |
| WebServer connection workers | WebServer Plugin | WebServer-owned Embassy task |
| Scheduler loop that emits scheduled Events | Scheduler Component | Event Router |
| Workflow event ingress and execution | Workflow Component | Event Router |
| Event Router RPC adapter | Contract-owning Component | Event Router |
| Internal service behind a typed capability | Capability provider | Embassy task when continuous work is required |

The owner is the layer that defines the work's behavior and lifecycle. A
Platform owns only Platform mechanisms; it does not spawn WebServer tasks just
because the WebServer consumes its network handle.

## Communication and execution are independent

Choosing an Event Router contract or a typed capability determines how
subsystems communicate. It does not automatically determine how their runtime
work is scheduled.

For example, WebServer provides a typed `WebServer` capability so dependent
Plugins can register routes. The WebServer Plugin then owns an Embassy task
that accepts connections and dispatches those registered routes. Neither the
typed capability nor its consumers require the server loop to be an Event
Router Component.

Conversely, a Plugin may provide typed capabilities and also own an Event
Router Component when it has separate Event Router-facing behavior. The
Component contains only that behavior; the Plugin's other services remain in
their owner-managed tasks.

## Startup and task access

The application Embassy entry owns the executor `Spawner`. Task-start access
flows from the composition root to the layer that owns each task:

~~~text
Embassy entry / Spawner
          |
          +--> Platform initializes Platform tasks
          +--> HAL composition initializes Driver tasks
          `--> System starts System and Plugin tasks
                         |
                         `--> Event Router runs its Components
~~~

Platform and HAL start only the runners for resources they construct. System
starts System-owned work and supplies a scoped, Embassy-backed task-start
boundary to Plugins. A Plugin starts its tasks after the complete Plugin graph
has registered, so typed capabilities and route registrations are present
before external traffic is accepted.

Long-lived fixed work uses `#[embassy_executor::task]` and, when required,
statically allocated task pools. It is not converted into boxed futures so
Event Router can poll it. Task and worker capacity belongs to the owning
subsystem's configuration.

Task start failure is a startup failure of the owning subsystem and propagates
through System startup. Event Router failure and completion govern Event Router
Components only; they do not silently become the lifecycle of independent
servers or Drivers.

## Invariants

- `Component::run` is reserved for Event Router-facing runtime work.
- Event Router is never used solely as a place to poll an arbitrary future.
- External protocol listeners and connection workers are owned by their
  protocol subsystem.
- Platform and HAL runners remain with the resources they drive.
- A task consumes handles or typed capabilities; it does not move business
  behavior into Platform merely to obtain a `Spawner`.
- Fixed long-lived tasks use Embassy's static task allocation and declared pool
  sizes.
- Plugin registration finishes before Plugin tasks accept external work.
- Event Router Components and owner-managed tasks have separate failure and
  lifecycle boundaries.

## Review checklist

Before loading a Component or adding a long-lived future, verify:

1. What Event Router Event, Workflow, RPC, or Agent contract does polling this
   future directly advance?
2. Would the future still be necessary without Event Router?
3. Which subsystem defines its behavior, capacity, and failure policy?
4. Can that owner run it as a statically allocated Embassy task or task pool?
5. Is Event Router being used only because it already polls futures?
6. Does startup guarantee that dependencies and registrations exist before the
   task begins accepting work?

If question 1 has no concrete answer, the future does not belong in Event
Router.
