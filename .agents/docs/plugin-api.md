# Plugin API

This is the authoritative contract for writing a Barracuda Plugin. Each topic
has exactly one approved way, and the
[`create-plugin`](../skills/create-plugin/SKILL.md) skill applies these rules
step by step. Code that disagrees with this document is migration work, not
precedent.

## Stability

Barracuda is 0.x. The Plugin API may change in any release, and there is no
compatibility layer, deprecation period, or version check for Plugins.

A change to the Plugin API lands as one change set: core, every affected
Plugin, this document, the `create-plugin` skill, and the
`cargo plugin sync --check` rules change together. The repository is never
left with some Plugins on the old way and some on the new one.

Compatibility guarantees become a goal only once Plugins are maintained
outside this repository.

## One way per topic

| Topic | The approved way |
| --- | --- |
| Construction | `pub fn new(context: &mut PluginContext<Builtins, Io>) -> Self`, nothing else |
| Platform and Board resources | Fields of `PluginContext`, assigned by System |
| Plugin-to-Plugin calls | A typed capability: `provide` in the provider, `require` in the consumer |
| Workflow integration | A Workflow Action or Event, with its schema under `schemas/` |
| Model-facing operations | An Agent Tool in an adapter Plugin |
| Lua-facing operations | A Lua package registered in the `vm` Plugin's `LuaPackageRegistry` |
| Portal page | `CaptivePortal::register_with_status` with a status, or `register` without one |
| Plugin HTTP API | The Plugin's own routes on `WebServer`; their paths and bodies are private to the Plugin and its page |
| Settings and credentials | Plugin-scoped KV storage |
| Files | The Plugin's `ScopedVfs`, on the mount chosen by the file's lifecycle |
| Bundled files | `filesystem/resources/` and `filesystem/workspace/resources/` |
| Documentation | `docs/plugin.md` from the template below |

## Directory layout

The framework owns every name at the root of `plugins/<id>/`. A Plugin uses
only the entries it needs and adds nothing else at the root:

```text
plugins/<id>/
├── plugin.toml                    # identity, dependencies, host tasks
├── crates/
│   ├── plugin/                    # package barracuda-<id>-plugin, the entry point
│   └── <crate>/                   # optional runtime or domain crates
│       └── resources/tools/<tool>/
│           ├── schema.json        # Agent Tool input schema
│           └── usage.md           # Agent Tool usage text
├── schemas/
│   ├── action/<address>/
│   │   ├── request.json
│   │   └── response.json
│   └── event/<event-id>.json
├── resources/                     # author-owned build inputs, such as web/
├── filesystem/                    # build outputs and assets bundled into the image
│   ├── resources/                 # -> the Plugin's /resources
│   └── workspace/resources/       # -> the shared /workspace/resources
└── docs/
    ├── plugin.md                  # required
    ├── action.md                  # required when schemas/action/ exists
    ├── event.md                   # required when schemas/event/ exists
    └── <topic>.md                 # optional, such as http.md
```

- `crates/plugin` holds the type annotated with
  `#[barracuda_plugin::macros::plugin]` and its `impl Plugin`. Add another
  crate only when a runtime or domain boundary owns substantial state,
  several contracts, a reusable API, or focused tests. Do not add a
  schema-only or transport-only crate.
- Files under `crates/<crate>/resources/` are compile-time inputs of that
  crate, such as Agent Tool definitions. They are never bundled.
- `resources/` holds sources the Plugin's build task turns into
  `filesystem/resources/`, such as `resources/web/entry.ts`. Authors organize
  its inside freely. It is unrelated to the runtime `/resources` mount.
- `filesystem/` never contains `data/`, `cache/`, `media/`, or a mutable
  subtree under `workspace/`.

## `plugin.toml`

`plugin.toml` is the single source of the Plugin's identity:

```toml
id = "example"
depends-on = ["webserver", "captive-portal"]
description = "One sentence on what the Plugin owns."
```

The ID and every `depends-on` entry match `^[a-z0-9]+(?:-[a-z0-9]+)*$`. The
`plugin` macro bakes them into `PluginDeclaration`; Rust code never repeats
them. Run `cargo plugin sync` after adding, renaming, or removing a Plugin.

### Host tasks

A Plugin that generates files declares a named host task:

```toml
[tasks.build]
cwd = "."
command = ["bun", "../../tools/web/build.ts"]
inputs = ["resources/web/entry.ts", "../../package.json", "../../bun.lock"]
outputs = ["filesystem/resources/entry.js"]
```

`cargo plugin run <task>` runs the task for every enabled Plugin that has it,
or for one Plugin with `--plugin <id>`. A Plugin whose resources must be
generated before compilation installs a `build.rs` that calls
`barracuda-plugin-build`; Cargo then runs its `build` task, fingerprinting the
declared inputs and outputs. Tasks run trusted repository code, never
re-enter Cargo, and never install tools.

Frontend tooling is repository-wide: one root `package.json`, `bun.lock`,
TypeScript configuration, formatter, and linter. Web sources and their tests
stay in the Plugin's `resources/web/`; shared builders and test helpers live
in `tools/web/`.

## Construction

Every Plugin has exactly this constructor, and System calls it as
`<Type>::new(&mut plugin_context)`:

```rust,ignore
impl ExamplePlugin {
    #[must_use]
    pub fn new<Builtins, Io>(context: &mut PluginContext<Builtins, Io>) -> Self {
        Self { clock: context.hal.peripherals.take_clock() }
    }
}
```

- The parameter is the only one. A Plugin needing a Platform or Board
  resource reads it from the context; there are no Plugin-specific
  arguments and no special cases in the generated registration.
- Use `const fn` when clippy asks for it. A Plugin generic over its device
  type may fix `Io` or bind `Builtins` with `where` clauses.
- Construction only moves resources out of the context. It never registers,
  spawns, or touches storage; that belongs to `register` and `start`.
- The Plugin never stores the context or a reference to it.

`PluginContext` carries only resources that the Platform, Board, or System
owns and assigns, never a feature's own services:

| Field | Use |
| --- | --- |
| `target_identity` | Copy |
| `ip_stack` | Copy |
| `http_clients` | Clone |
| `receive_slots` | Clone; leases connections for long-lived receive loops |
| `entropy` | Clone |
| `wifi` | `take()` by the one Plugin that owns Wi-Fi policy |
| `hal.peripherals` | `take_<peripheral>()` by the one Plugin that owns the peripheral |
| `hal.exposed_io` | Clone the `Arc` |

A new field is a Plugin API change and needs a resource that System owns
for the whole Target. A capability that some Plugin can own is published by
that Plugin instead.

## Lifecycle

`Plugin::REQUIREMENTS` declares the filesystem the Plugin needs:
`PluginFilesystem::None` (the default), `Private`, or `Inspect`. Portal
entries, file storage, and bundled resources need `Private`.

`Plugin::register` runs synchronously in dependency order. It:

1. requires capabilities from Plugins listed in `depends-on`;
2. constructs the complete capability and service graph;
3. publishes every provided capability with `provide`;
4. installs Workflow Actions, Agent Tools, Lua packages, portal entries, and
   HTTP routes;
5. keeps every registration guard alive with `context.retain(...)`.

`Plugin::start` runs after every Plugin has registered. It only spawns
long-running work through `context.task_spawner()`, giving every permanent
task its own `context.task_token()`. It cannot publish capabilities. Task
ownership, capacity, and cancellation follow
[`execution-ownership.md`](execution-ownership.md).

Plugin Manager owns rollback and unload. A Plugin never adds its own
lifecycle or global registry.

## Collaboration contracts

Choose the contract by the caller:

| Caller | Contract |
| --- | --- |
| Another Plugin | Typed capability |
| A Workflow | Workflow Action, or Workflow Event for an asynchronous fact |
| The model | Agent Tool |
| Lua code in the VM | Lua package |

One service may expose several of these views. The typed capability stays the
direct machine-oriented API. Every other view is a thin adapter over it, and
provider-only mutation handles stay private to the provider.

### Typed capabilities

The provider publishes `Rc<T>` with `PluginRegisterContext::provide`. The
consumer lists the provider in `depends-on` and obtains the same value with
`require::<T>("<provider-id>")`. No JSON, string lookup, or runtime discovery
sits between them.

### Workflow Actions

An Action implements `WorkflowActionHandler`, registers in
`WorkflowActionRegistry`, and embeds its schemas with
`workflow_action_schema!("<address>")`, which reads
`schemas/action/<address>/request.json` and `response.json`:

```rust,ignore
impl WorkflowActionHandler for SendMessage {
    type Request = SendRequest;
    type Response = SendResponse;

    const SCHEMA: WorkflowActionSchema = workflow_action_schema!("message.send");

    fn invoke(&self, request: SendRequest) -> WorkflowActionFuture<'_, SendResponse> {
        Box::pin(async move { send(request).await })
    }
}
```

The runtime validates the request before decoding it and the response before
returning it. A success with no value is `{}`. A stable business failure is
an ordinary response such as `{"error":"not_found"}` and appears in the
response schema; `WorkflowActionError` is only for infrastructure failures.

### Workflow Events

An Event type implements `Event` with a stable `ID` and is emitted through
`WorkflowService::emit`, or `emit_to` when a Topic is part of matching. Its
payload is a bounded Serde value described by `schemas/event/<event-id>.json`.
A logical stream is a sequence of Events carrying correlation, ordering, and a
terminal outcome, with capacity and backpressure owned by the producer.

### Agent Tools

A Tool registers through `AgentToolRegistry`. Any Tool beyond the Agent's
own lives in an adapter Plugin named `agent-<name>`, which translates the
call into the provider's typed capability or a Workflow operation, so the
provider stays usable without `agent`. Its definition lives
beside the crate that registers it, in
`crates/<crate>/resources/tools/<tool>/schema.json` and `usage.md`.
Prompting, response encoding, and Tool errors stay in the adapter, and Tools
follow [`agent-oriented-api-design.md`](agent-oriented-api-design.md).

### Lua packages

A Lua package beyond the VM's built-ins registers in the
`LuaPackageRegistry` required from `vm`, from a Plugin named `vm-<name>`. Dropping the registration revokes the package's
callbacks and invalidates its handles.

## Portal and HTTP

A Plugin with a configuration page registers one `WebEntry` with
`CaptivePortal`:

- `register_with_status(entry, files, || EntryStatus)` when its card shows a
  status. The closure is synchronous and cheap: it reads state the Plugin
  already holds, never touches a device or the network, and never calls the
  portal.
- `register(entry, files)` when it has no status.

`files` is `ResourceFiles::from(context.filesystem()?.clone())`, serving the
entry's module and assets from the Plugin's `/resources`. The page's sources
live in `resources/web/` and build into `filesystem/resources/`.

A Plugin's own HTTP API is served with `WebServer::serve_http` and retained
like any registration. Its paths, methods, and bodies are the Plugin's
private contract with its page, documented in `plugin.md` or `http.md`. A
route never returns credentials.

## Storage and files

Classify state by lifecycle, not size:

| State | Home |
| --- | --- |
| Settings, credentials, and small structured state | Plugin-scoped KV storage, `context.storage()` |
| Files required for correctness | `/data` |
| Reproducible files System may delete | `/cache` |
| Large durable runtime content | `/media` |
| Files shipped with the image | `/resources`, read-only |
| Immutable files shared by every Plugin | `/workspace/resources`, read-only |
| Files exchanged between Plugins for a run | `/workspace/cache` |
| Shared files that outlive their run | `/workspace/media` |
| Currently inserted removable media | `/workspace/removable/<slot-id>` |

A Plugin receives an ordinary `ScopedVfs` from `context.filesystem()`. It
never mounts, selects a filesystem, reaches System paths, or touches another
Plugin's namespace, and it never writes at `/` or `/workspace` directly. Shared
state required for correctness keeps one owner and lives in that owner's KV
storage or `/data`.

Mount policy enforces the contract: writes below `/resources` and
`/workspace/resources` fail with `ReadOnly`, a rename across mounts fails with
`CrossMount`, paths cannot escape the scope, and unload keeps `/data`,
`/media`, and `/workspace/media`. `/cache` and `/workspace/cache` may vanish
at any time. Mutable defaults are read from `/resources` and copied into
`/data` only when absent.

Bundled files map directly:

```text
plugins/<id>/filesystem/resources/index.html
-> /resources/index.html                     (the Plugin's own view)
plugins/<id>/filesystem/workspace/resources/models/common.bin
-> /workspace/resources/models/common.bin   (every Plugin's view)
```

Shared contributions merge by path, and a collision fails the image build. How
System backs these mounts is in
[`platform-architecture.md`](platform-architecture.md#filesystems-and-database).

## Plugin documentation

Every Plugin has `docs/plugin.md` in this form:

````markdown
# <Name> Plugin

<One or two sentences on what the Plugin owns.>

- Plugin ID: `<id>`
- Dependencies: `<id>`, `<id>`
- Context resources: `<field>`, `hal.peripherals.take_<peripheral>()`
- Provided capabilities: `<crate>::<Type>`
- Required capabilities: `<crate>::<Type>` from `<id>`
- Workflow Actions: `<address>`; see `action.md`
- Workflow Events: `<event-id>`; see `event.md`
- Agent Tools: `<tool>`
- Lua packages: `<package>`
- Portal entry: `<entry-id>`, with status
- HTTP routes: `GET <path>`, `PUT <path>`
- Tasks: <each owned long-running task and its capacity>
- Storage: <KV keys and file paths>

<Free-form sections, in any order, for everything else.>
````

- All thirteen bullets appear in this order, before the first `##` heading,
  each starting at the beginning of a line. Write `none` for an empty one. A
  value may wrap onto indented continuation lines.
- `Plugin ID` and `Dependencies` match `plugin.toml` exactly.
- Describe the current contract only. History belongs in version control.

`action.md` lists every Action address, its request and response, its stable
business errors, and its schema path. `event.md` lists every Event ID, its
payload, schema path, Topic, emission condition, and bounds.

## Checks

`cargo plugin sync --check`, which runs in CI, rejects a Plugin that breaks
these rules:

- the entry type's constructor is not
  `new(context: &mut PluginContext<..>)`;
- the Plugin root contains a name outside the layout above;
- `schemas/` contains anything except `action/<address>/{request,response}.json`
  and `event/<event-id>.json`;
- `docs/plugin.md` is missing, lacks a summary bullet, has them out of order,
  or disagrees with `plugin.toml` on the ID or dependencies;
- `docs/action.md` or `docs/event.md` is missing while the matching schemas
  exist.

The check covers only these shapes. It does not version Plugins or judge
anything else.
