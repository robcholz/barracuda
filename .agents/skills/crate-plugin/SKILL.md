---
name: crate-plugin
description: Create or update a Barracuda Plugin crate with its fixed entry layout, required plugin.md identity document, optional inline or standalone Component, RPC and event contracts, capabilities, and schema-baked wire types.
---

# Crate Plugin

Add integrations as system-managed Plugins under `plugins/`. A Plugin is the
unit registered and started by `barracuda-system`; it may own zero or more Event Router
Components and may provide or require typed capabilities. Every integration
has a Plugin crate even when its Component remains a separate crate.

Follow [.agents/docs/codestyle.md](../../docs/codestyle.md) for Rust API and
lint conventions. Follow
[.agents/docs/platform-architecture.md](../../docs/platform-architecture.md)
for platform capability boundaries. A Plugin must never implement support for
a concrete Platform type or depend on a concrete Platform crate.

## Layout

Create this fixed structure for `<my-plugin>`:

```text
plugins/<my-plugin>/
├── plugin.toml                  # required selection metadata
├── crates/
│   ├── plugin/
│   │   ├── Cargo.toml
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── component.rs  # only for a small Event Router Component
│   │       └── task.rs       # only for a small Plugin-owned Embassy task
│   └── component/                 # only for a substantial Component
│       ├── Cargo.toml
│       └── src/
└── docs/
    ├── plugin.md               # required
    ├── rpc.md                  # only when this Plugin exposes RPCs
    └── event.md                # only when this Plugin emits events
```

- `crates/plugin` is the Plugin implementation crate. Its package name is
  `barracuda-<my-plugin>-plugin`.
- `plugin.toml` is required and is the single source of truth for the stable
  Plugin ID, direct Plugin dependencies, and one concise non-empty description
  of at most 80 characters. IDs and dependencies must not contain leading or
  trailing whitespace. `cargo plugin select` shows the description beside the
  Plugin name, and discovery fails when the file is missing or invalid.
- `crates/plugin/src/lib.rs` defines `XxxPlugin` and implements
  `barracuda_plugin_manager::Plugin`.
- Put a small Component in `crates/plugin/src/component.rs`. Keep a substantial
  Component in `crates/component` when it has enough RPC modules, state,
  reusable API, or tests that inlining it would make the Plugin crate hard to
  read. The Plugin crate depends on and loads that Component; it never replaces
  the Plugin entry point.
- A standalone Component package is named `barracuda-<name>-component`.
- A Plugin with no Component is valid; capability-only Plugins keep only the
  Plugin implementation and the modules needed by that capability.
- Put a small owner-managed Embassy task in `task.rs`. A task is independent of
  whether the Plugin also owns an Event Router Component.
- Put additional implementation crates, such as `wire`, under
  `plugins/<my-plugin>/crates/` beside `plugin`.
- The root workspace discovers `plugins/*/crates/*` automatically. After
  creating the Plugin, run `cargo plugin sync`; this discovers its metadata,
  package, and entry type, then adds it to System. Plugin Manager scans the complete
  `PluginDeclaration::DEPENDS_ON` graph and chooses the registration order at runtime.
  Never edit the generated Plugin blocks by hand. `barracuda-system` watches
  `plugins/` from its build script and rejects a stale registry with this same
  command. The build script deliberately validates rather than rewriting the
  manifest because Cargo resolves dependencies before running build scripts.

## Plugin implementation

Give every Plugin a stable static declaration. In `lib.rs`:

- define and document `XxxPlugin`;
- apply `#[barracuda_plugin_api::plugin]` to the `XxxPlugin` type so its
  `PluginDeclaration` implementation is baked from `plugin.toml`;
- never duplicate the ID or dependency list in Rust constants;
- implement the synchronous `register` phase to construct the Plugin's complete
  capability and Component graph; keep the default no-op only when the Plugin
  owns no registration-time resources;
- explicitly load every owned Component during `register` with
  `context.event_router.load(component)`;
- use the optional synchronous `start` phase after every Plugin has registered;
  `PluginStartContext` deliberately cannot load Components or publish
  capabilities, but exposes the System-installed Embassy spawner for starting
  Plugin-owned tasks;
- use `PluginRegisterContext::require` and `provide` for typed cross-Plugin
  capabilities;
- use the Plugin's scoped storage directly when persistent state is needed;
- retain registration guards with `PluginRegisterContext::retain` so unload reverses
  external registrations;
- never defer capability publication, route registration, or Component loading
  to `start`.

`barracuda-system` first adds the complete Plugin set, then calls
`PluginManager::register_all` so Plugin Manager scans the dependency DAG and
registers it in dependency order. System calls `PluginManager::start` once
afterward. Never start one Plugin between registrations. Both lifecycle phases are synchronous:
`register` atomically mutates the capability and Component graph, while `start`
synchronously starts owner-managed tasks. Async I/O and long-running work do
not run inside either lifecycle method. A long-running future belongs in an
Event Router Component only when it directly advances an Event Router
contract. Other service futures run as Plugin-owned Embassy tasks started
through `PluginStartContext::task_spawner`. Follow
[execution-ownership.md](../../docs/execution-ownership.md) for this boundary.

The Plugin owns construction and defaults for its Components. Every Plugin
constructor receives the shared `PluginContext` assembled by System and takes
or clones the public semantic handles it owns. The selected Platform initializes
those handles; a Plugin must not receive or construct concrete Platform
implementations.
`barracuda-system` registers the fixed Plugin set and must not assemble
Component internals.

## Component and contract design

When the Plugin has an inline Component, keep `component.rs` focused on Event
Router registration and Event Router-facing lifecycle. Keep unrelated socket,
Driver, and service loops in owner-managed Embassy tasks. For a standalone
Component crate, keep the same separation in its `src/component.rs`. Put one
RPC module per address and expose reusable `*_handler` constructors from those
modules.

Design RPCs and events like a REST API: expose the minimal caller-independent
surface and match existing naming and message shapes.

- Prefer named `enum` and `struct` types over raw byte arrays.
- Text serializes as a string. Fixed text uses a named, UTF-8, NUL-terminated
  newtype with fixed capacity.
- Use bare `u8` arrays only for genuinely opaque bytes.

## Documentation

Create Plugin and contract documentation with the Plugin:

- `plugins/<my-plugin>/docs/plugin.md` is required. It states the stable Plugin
  ID exactly as declared by `PluginDeclaration::ID`, lists the direct Plugin dependencies
  exactly as declared by `PluginDeclaration::DEPENDS_ON` (write `none` when empty), lists
  every provided typed capability by its exact public Rust type name (write
  `none` when empty), and explains the Plugin's purpose and responsibilities.
  When applicable, also list its owned Components and required typed
  capabilities.
- `plugins/<my-plugin>/docs/rpc.md` lists every RPC address, request, response,
  and method error variant when the Plugin exposes RPCs.
- `plugins/<my-plugin>/docs/event.md` lists every emitted event, message type,
  cardinality, and emission condition when the Plugin emits events.

Never omit `plugin.md`. Omit `rpc.md` or `event.md` when the Plugin exposes no
such contract.

## rpc_dynamic and schema

Add runtime-dynamic JSON/wire support only when explicitly requested.
`#[rpc_dynamic]` can use fixed-layout request and response types in the Plugin
crate.

When a `build.rs` bakes request schemas, put shared DTOs in
`plugins/<my-plugin>/crates/wire` with package name
`barracuda-<my-plugin>-wire`; the Plugin and build script both depend on that
wire crate.

## High-frequency mappings

Model a high-frequency typed integration as a Plugin. Create
`plugins/<mapping-name>/crates/plugin`; implement its mapping Component inline
or in `crates/component` according to the size rule, and declare dependencies
on the Plugins whose contracts it connects.
