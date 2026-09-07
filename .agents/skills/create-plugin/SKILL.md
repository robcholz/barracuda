---
name: create-plugin
description: Create, migrate, or update a Barracuda Plugin with typed capabilities, Workflow Actions and Events, Agent Tools, lifecycle ownership, schemas, and focused tests.
---

# Create Plugin

Build Plugins around current runtime boundaries. Inspect implementation and real
callers before changing a contract; code is authoritative when existing Plugin
docs disagree with it.

## Architecture boundaries

Choose the communication mechanism from the caller relationship:

- Use a typed Plugin capability for direct Plugin-to-Plugin collaboration.
  Publish it during `Plugin::register` with `provide`, and obtain it from a
  dependency declared in `plugin.toml` with `require`.
- Use a Workflow Action for an operation called by the Workflow DSL. Implement
  `WorkflowActionHandler` and register it in `WorkflowActionRegistry`.
- Use a Workflow Event for an asynchronous fact matched by Workflow. Implement
  `Event` and emit a bounded Serde value through `WorkflowService`.
- Use an Agent Tool for a model-facing operation. Register it through
  `AgentToolRegistry`, normally in a small Agent adapter Plugin that calls the
  provider's typed capability.

A Plugin may expose one underlying service through deliberate typed and
Workflow views. Keep the capability machine-oriented and the Workflow contract
stable and serializable. Provider-only mutation handles stay private to the
provider and its registration API.

## Layout

Use only the parts required by the Plugin:

```text
plugins/<plugin>/
├── plugin.toml
├── filesystem/                   # only home for bundled runtime assets
│   └── resources/                # only supported bundled subtree today
├── crates/
│   ├── plugin/
│   │   ├── Cargo.toml
│   │   └── src/lib.rs
│   └── runtime/                  # optional substantial runtime/domain boundary
├── schemas/
│   ├── action/
│   │   └── <action-address>/
│   │       ├── request.json
│   │       └── response.json
│   └── event/
│       └── <event-id>.json
└── docs/
    ├── plugin.md
    ├── action.md                 # when Workflow Actions exist
    └── event.md                  # when Workflow Events exist
```

The Plugin implementation package is `barracuda-<plugin>-plugin`. Keep small
services beside the Plugin entry point. Add `crates/runtime` only when a real
runtime or domain boundary owns substantial state, multiple contracts,
reusable APIs, or focused tests. Do not create a schema-only or transport-only
crate.

`plugin.toml` is the source of truth for the stable Plugin ID, direct Plugin
dependencies, and concise description. Apply
`#[barracuda_plugin::macros::plugin]` to the Plugin type rather than duplicating
that metadata in Rust. Run `cargo plugin sync` when adding or renaming a Plugin,
changing its selection metadata, or otherwise changing generated System
registry inputs.
Plugin IDs and every `depends-on` entry must match
`^[a-z0-9]+(?:-[a-z0-9]+)*$`; each identity is a portable lowercase path
segment, not a path.

## Plugin filesystem

When a Plugin reads, writes, or contributes files, read and follow
[`filesystem.md`](filesystem.md). A Plugin receives an ordinary `ScopedVfs`;
do not introduce a Plugin-specific VFS type or expose mount-table ownership.

## Lifecycle ownership

In `register`:

- construct the complete capability and service graph;
- publish every provided capability;
- require only capabilities from declared Plugin dependencies;
- install Workflow Actions, Agent Tools, routes, and other registrations;
- retain every external registration guard with `context.retain(...)`.

In `start`, spawn Plugin-owned long-running work through the installed Embassy
spawner. Capability publication and registration belong to `register`, before
tasks accept external work. Give every permanent task its own
`PluginTaskToken` and make each await path cancellation-safe.

Plugin Manager owns rollback and unload. Do not add a parallel lifecycle or
global registry.

## Workflow Action contracts

Define one typed handler per address:

```rust,ignore
struct SendMessage;

impl WorkflowActionHandler for SendMessage {
    type Request = SendRequest;
    type Response = SendResponse;

    const SCHEMA: WorkflowActionSchema = workflow_action_schema!("message.send");

    fn invoke(&self, request: SendRequest) -> WorkflowActionFuture<'_, SendResponse> {
        Box::pin(async move { send(request).await })
    }
}
```

Store schemas at
`plugins/<plugin>/schemas/action/<address>/{request,response}.json` and include
them with `workflow_action_schema!`. The Workflow runtime validates the request
before Serde decoding and validates the serialized response before returning it
to the execution.

Keep request and response documents natural for callers. A successful
operation with no return value serializes `{}`. Stable business failures are
ordinary response documents such as `{"error":"not_found"}` and appear in the
response schema. Reserve `WorkflowActionError` for infrastructure failures.

## Events and streaming

Declare Events as typed identities matched by stable Event IDs. Producers
serialize bounded input values and call `WorkflowService::emit` or
`WorkflowService::emit_to` when a Topic is part of the matching contract.

An application-level stream is a sequence of bounded Events carrying
correlation, ordering, and a terminal outcome. Define explicit capacity and
backpressure in the subsystem that owns the stream.

## Agent Tools

Agent Tools are separate from Workflow Actions. A Tool declares its public
model-facing schema and usage text under the adapter Plugin's
`crates/plugin/resources/tools/<tool>/` directory. The adapter translates the
Tool request into the provider's typed capability or Workflow operation and
retains the registration guard for its Plugin lifetime.

Keep Agent prompting, response encoding, and Tool-specific errors in the
adapter. Provider Plugins remain usable without Agent.

## Migration and verification

When migrating an old contract:

1. Identify the business operation and every real caller.
2. Assign direct Plugin callers to a typed capability, Workflow callers to an
   Action or Event, and model callers to an Agent Tool.
3. Remove the superseded transport types, adapters, package names, schemas,
   docs, and negative compatibility assertions.
4. Update callers, tests, and examples to use the production contract.
5. Search the whole repository for the removed vocabulary and paths.

Use focused tests for the affected Plugin. Cover exact Action addresses and
schemas, request and response validation, business failures, capability
publication and consumption, retained registrations, and task cancellation as
applicable. Parse schema files, run formatting, targeted tests, clippy, and
`git diff --check` before handoff.

## Documentation

Keep docs synchronized with implementation:

- `plugin.md` states the exact Plugin ID, direct dependencies, provided and
  required capability type names, Workflow Actions, Events, Agent Tools, owned
  tasks, and responsibility.
- `action.md` states every address, request/response document, stable business
  errors, and schema path.
- `event.md` states every Event ID, JSON input, emission condition, bounds, and
  application-level stream semantics.

Describe the current contract directly. Migration history belongs in version
control, not in caller-facing Plugin documentation.
