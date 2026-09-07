---
name: create-plugin
description: Create, migrate, or update a Barracuda Plugin with typed capabilities, Agent/Workflow-facing JSON RPCs, JSON Events, lifecycle ownership, schemas, and focused tests.
---

# Create Plugin

Build Plugins around current runtime boundaries. Inspect implementation and real
callers before changing a contract; do not use existing Plugin docs as evidence
when they disagree with code.

## Architecture boundaries

Choose the communication mechanism from the application relationship:

- Use a typed Plugin capability for direct Plugin-to-Plugin collaboration.
  Publish it during `Plugin::register` with `provide`, and obtain it from a
  dependency declared in `plugin.toml` with `require`.
- Use a JSON RPC for an operation intended to be called by an Agent or Workflow.
  These endpoints use visibility `"*"`.
- Reserve visibility `"system"` for Event Router infrastructure such as Event
  ingress and Workflow control. A normal Plugin must not register into this
  visibility.
- Use JSON Events for asynchronous facts and application-level streams. A
  stream is a sequence of bounded Events carrying correlation, ordering, and a
  terminal outcome; one large JSON array is batching, not streaming.
- Native RPC remains a low-level fixed-layout transport. Do not use it as a
  substitute for a typed capability or as an Agent/Workflow contract unless the
  user explicitly selects that design.

A Plugin may expose the same underlying service through two deliberate views:
a typed capability for other Plugins and a JSON RPC for Agents/Workflows. Keep
the capability machine-oriented and the JSON contract caller-oriented. Do not
make provider-only mutation handles available through a read-only capability.

## Layout

Use the parts required by the Plugin:

```text
plugins/<plugin>/
├── plugin.toml
├── filesystem/                   # optional prebuilt scoped files
│   └── resources/                # runtime read-only files
├── crates/
│   ├── plugin/
│   │   ├── Cargo.toml
│   │   └── src/lib.rs
│   └── component/                 # substantial Event Router Component only
├── schemas/
│   └── rpc/
│       └── <short-rpc-name>/      # only when the Plugin exposes JSON RPC
│           ├── request.json
│           └── response.json
└── docs/
    ├── plugin.md
    ├── rpc.md                     # only when RPCs exist
    └── event.md                   # only when Events exist
```

The Plugin implementation package is `barracuda-<plugin>-plugin`. Keep a small
Component beside the Plugin entry point; use `crates/component` when it owns
substantial state, multiple contracts, reusable APIs, or focused tests. Extra
crates must represent real runtime or domain boundaries. Do not create a wire
crate solely to bake JSON schemas.

`plugin.toml` is the source of truth for the stable Plugin ID, direct Plugin
dependencies, and concise description. Apply `#[barracuda_plugin_api::plugin]`
to the Plugin type rather than duplicating that metadata in Rust. Run
`cargo plugin sync` when adding or renaming a Plugin, changing its selection
metadata, or otherwise changing the generated System registry inputs.

## Plugin filesystem

When a Plugin reads, writes, or contributes files, read and follow
[`filesystem.md`](filesystem.md). A Plugin receives an ordinary `ScopedVfs`;
do not introduce a Plugin-specific VFS type or expose mount-table ownership.

## Lifecycle ownership

In `register`:

- construct the complete capability and Component graph;
- publish every provided capability;
- require only capabilities from declared Plugin dependencies;
- load every owned Component with `context.event_router.load(...)`;
- retain external registration guards with `context.retain(...)`.

In `start`, only start Plugin-owned long-running tasks through the installed
Embassy spawner. Do not publish capabilities, register routes, or load
Components there. Event Router Components own futures that directly advance
Event/Workflow contracts; unrelated service loops are Plugin-owned tasks.

Plugin Manager owns rollback and unload. Do not add parallel ad-hoc lifecycle
or global registries.

## JSON RPC contracts

Define one marker per address:

```rust
pub struct Now;

impl JsonRpcSchema for Now {
    const ADDRESS: &'static str = "time.now";
    const REQUEST_SCHEMA: JsonSchema = json_schema!("now", request);
    const RESPONSE_SCHEMA: JsonSchema = json_schema!("now", response);
    const MAX_REQUEST_BYTES: usize = 2;
    const MAX_RESPONSE_BYTES: usize = 34;
}
```

Store schemas at
`plugins/<plugin>/schemas/rpc/<short-rpc-name>/{request,response}.json`. The
short name is local to the Plugin and should use snake case. Include schemas at
compile time with `json_schema!`; do not use a build script, schema registry,
`rpc_dynamic`, or schema-baking feature.

Register with `register_json::<Method, _>("*", handler)`. A handler receives one
lane-backed `JsonRef` and one consuming `JsonWriter`; JSON RPC is unary request
to unary response. It must validate the request shape because registration
metadata does not perform runtime JSON Schema validation. Write the response
directly into the lane through `JsonPayload` where practical, and allocate only
when the business operation needs owned data.

Keep request and response documents natural for callers. A successful operation
with no return value writes `{}`. Stable business failures are JSON response
documents such as `{"error":"not_found"}` and must appear in the response
schema. Use `RpcError` only for transport, framing, malformed JSON, capacity,
and runtime failures.

Set `MAX_REQUEST_BYTES` and `MAX_RESPONSE_BYTES` explicitly from bounded
application needs. Do not hide an unbounded operation behind a large arbitrary
frame size.

## Events and streaming

Declare Events as JSON input contracts matched by stable Event IDs. Keep every
Event bounded. For application-level streaming, define bounded chunk and
terminal Events, normally with fields such as `run_id`, `sequence`, and the
chunk or completion result. Producers must handle Event capacity exhaustion;
do not accumulate an unbounded stream in Event Router memory.

Do not add transport cardinality to JSON RPC or Workflow steps. If native
transport streaming is explicitly required, keep it outside the Agent/Workflow
JSON contract and document why a long-lived RPC lane is acceptable.

## Migration and verification

When migrating a legacy Plugin RPC:

1. Identify the business operation and every real caller.
2. Decide which callers should use a typed capability and which need JSON RPC.
3. Remove legacy `rpc_dynamic`, `rpc_message`, schema baking, and schema-only
   wire crates from the migrated provider.
4. Do not keep a native compatibility endpoint at the same address. Either
   migrate direct consumers in scope or report the intentional intermediate
   compile break for their later Plugin migration.
5. Update Plugin-owned tests and examples to use the production JSON or
   capability API.

Use focused tests for the affected Plugin rather than starting with the whole
workspace. Cover the exact address and schemas, visibility, valid JSON calls,
business failures, malformed and oversized input, response bounds, capability
publication/consumption, and Plugin load/unload behavior as applicable. Check
schema files with a JSON parser and run formatting, targeted tests, clippy, and
`git diff --check` before handoff.

## Documentation

Keep docs synchronized with implementation:

- `plugin.md` states the exact Plugin ID, direct dependencies, provided and
  required capability type names, owned Components, tasks, and responsibility.
- `rpc.md` states every address, visibility, request/response documents, stable
  business errors, byte limits, and schema paths.
- `event.md` states every Event ID, JSON input, emission condition, bounds, and
  application-level stream semantics.

Do not describe deleted wire DTOs, build-time schema baking, or native
cardinality for a JSON contract.
