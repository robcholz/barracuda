# Plugin Communication

Event Router is designed primarily for Workflow and Agent call contracts.
Other Plugin-to-Plugin integrations generally use typed capabilities. Treat
this as design guidance rather than a prohibition: choose the mechanism that
best matches the Plugin's ownership, lifecycle, and contract requirements.

System-owned Platform and HAL resources are fixed construction inputs. System
passes them directly when constructing the concrete Plugin that owns their
use. `PluginContext` capability lookup is reserved for capabilities published
by declared Plugin dependencies.

Communication mechanism and execution ownership are separate decisions. See
[`execution-ownership.md`](execution-ownership.md) before placing a long-lived
future in `Component::run`.

## Plugin documentation

Every `plugins/<plugin>/docs/plugin.md` must state the typed capabilities that
the Plugin provides. Use the exact public Rust type names. When the Plugin
provides no typed capability, write `none` explicitly.

Use this field in the Plugin summary:

```text
- Provided typed capabilities: `CapabilityType`
```

or:

```text
- Provided typed capabilities: none
```

## Plugin lifecycle

`Plugin::register` synchronously constructs the complete Plugin graph without
yielding. A Plugin publishes and requires typed capabilities, installs retained
registrations, and explicitly loads each owned Event Router-facing Component through
`context.event_router.load(component)` in this phase. A background service that
does not directly advance an Event Router contract is an owner-managed Embassy
task, not a Component.

`Plugin::start` is only an optional synchronous post-registration hook. Its
`PluginStartContext` cannot publish capabilities or load Components. Event
Router begins polling loaded Components only after registration and startup
hooks complete. Plugin-owned tasks obtain the System-installed Embassy spawner
from `PluginStartContext::task_spawner` and start only after the complete Plugin
graph has registered.
