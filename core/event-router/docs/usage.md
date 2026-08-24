# Event Router Usage

The Event Router runs a set of Components on a fixed-capacity, full-duplex,
`no_std` cooperative runtime, and drives durable Workflows that chain RPC calls
when an Event arrives. This guide covers the caller side: defining messages and
methods, registering a Component, making calls, emitting Events, and loading
Workflows. The boundary between Event Router Components and owner-managed
Embassy tasks lives in
[`execution-ownership.md`](../../../.agents/docs/execution-ownership.md).

## Quick start

```rust
let lanes = Box::leak(Box::new(RpcLaneStorage::<N, M, Q>::new()));
let mut router = EventRouter::new(lanes).await?;

router.load(Box::new(MyComponent))?;
// Poll `router` from the cooperative executor.
```

System mounts the process-wide VFS before constructing Event Router. Event
Router uses that global namespace directly and owns only
`/system/workflows.json`, which contains the ordered Workflow definitions as
one JSON array. Other System services may use sibling paths under `/system`.

`N` is the number of RPC lanes, `M` the frame size in bytes, and `Q` the
number of waiters per lane.

## 1. Define a message DTO

A message is a fixed-layout type. The serde derives describe its JSON shape, the
zerocopy derives describe its wire bytes, and `RpcWire` records the byte region
of each field so Workflow links can copy individual fields.

```rust
#[repr(C)]
#[derive(
    Serialize, Deserialize, Clone, Copy, Debug,
    Immutable, IntoBytes, KnownLayout, TryFromBytes, RpcWire,
)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
struct SetLevelRequest {
    session: u32,
    level: u32,
}
```

For the common case, `#[rpc_message]` writes that whole derive list for you:

```rust
#[repr(C)]
#[rpc_message]
#[derive(Clone, Copy, Debug)]
struct SetLevelRequest {
    session: u32,
    level: u32,
}
```

The macro includes `RpcWire` for named-field structs and leaves it off enums.
Keep `serde` and `zerocopy` as dependencies of the crate.

## 2. Declare an RPC method

```rust
struct SetLevel;

#[rpc_dynamic]
impl RpcMethod for SetLevel {
    const ADDRESS: &'static str = "session.set_level";
    type Request = SetLevelRequest;
    type Response = ();
    type Error = SetLevelError;
    type Input = Unary;      // request cardinality
    type Output = Unary;     // response cardinality
}
```

`Input` and `Output` are `Unary` or `Streaming`. Annotate the impl with
`#[rpc_dynamic]` to make the method reachable through JSON and field-name
Workflow links.

## 3. Register a component

```rust
impl Component<M> for MyComponent {
    fn register(&mut self, context: &mut RegisterContext<'_, M>) -> ComponentResult<()> {
        context.register_rpc::<SetLevel, _>(|_context, request: RpcFrame<SetLevelRequest>| async move {
            Ok(Ok(())) // or Ok(Err(SetLevelError::...))
        })?;
        Ok(())
    }
    // run / unregister as needed
}
```

A handler receives the call's `RpcContext` and an `RpcFrame<Request>`. Streaming
inputs arrive as an `RpcStream<RpcFrame<Request>>`.

## 4. Call RPCs

```rust
// Typed
let outcome = client.call::<SetLevel>(SetLevelRequest { session: 7, level: 2 })?.await?;

// JSON (available on #[rpc_dynamic] methods)
let value = client
    .call_json(&RpcAddress::try_from(SetLevel::ADDRESS)?, &serde_json::json!({
        "session": 7, "level": 2
    }))
    .await?;
// -> {"ok": true, "value": ...} or {"ok": false, "error": ...}

// Raw wire
let (writer, reader) = client.call_payload(&address)?;
```

## 5. Events and Workflows

### Emit an Event

```rust
struct GatewayMessage;

impl Event for GatewayMessage {
    const ID: &'static str = "gateway.message.received";
    type Message = [u8; 4];
    type Input = Unary;
}

EventEmitter::<M>::new(context.rpc().clone())
    .emit::<GatewayMessage>([1, 2, 3, 4])
    .await?;
```

An Event may also carry a Topic. Topics contain 1–16 ASCII letters, digits,
`_`, `-`, or `.`:

```rust
let topic = Topic::try_from("gateway-1")?;
EventEmitter::<M>::new(context.rpc().clone())
    .emit_to::<GatewayMessage>(&topic, [1, 2, 3, 4])
    .await?;
```

### Load a Workflow

```rust
let client = WorkflowClient::<M>::new(context.rpc().clone());
client.load(r#"{
    "id": "demo",
    "match": { "event": "gateway.message.received" },
    "steps": [
        { "call": "integration.add-one" },
        { "call": "integration.record", "arguments": { "token": "$previous.output.token", "extra": 5 } }
    ]
}"#).await?;
```

`match.topic` is an optional exact selector within the 16-byte bound:

```json
{
  "id": "gateway-1-demo",
  "match": {
    "event": "gateway.message.received",
    "topic": "gateway-1"
  },
  "steps": [
    { "call": "integration.add-one" }
  ]
}
```

Matching is `event AND exact topic` when `topic` is present. Without `topic`,
the Workflow keeps the previous behavior and accepts every matching Event ID,
including Events emitted with a Topic. An Event emitted without a Topic cannot
match a Workflow that requires one. `*` is rejected in a Topic; omitting the
field already expresses the wildcard behavior. All matching Workflows fan out.

Step 0 is the ingress step: the Event payload becomes its request. Each later
step describes how to build its request from the previous response, based on
`arguments`:

| `arguments` | Link |
| --- | --- |
| omitted | `Direct` — the previous response passes through byte-for-byte. |
| present, literals only | `Literal` — the request comes entirely from the arguments. |
| present, with a `$` reference | `Mapping` — literals plus fields copied from the previous response. |

A reference looks like `$previous.output.<field>` and names a single top-level
serde field. For a `Mapping` link, give the destination field `#[serde(default)]`
or a placeholder in the literal arguments, so the JSON codec can fill it from
the reference.

### Unload a Workflow

```rust
client.unload(&WorkflowId::try_from("demo")?).await?;
```

## 6. Bake JSON Schemas

The `schema` feature exposes the host-side bake pipeline through the facade.
Add a `build.rs` that writes each registered schema into `OUT_DIR` and enables
the `rpc_schema_baked` cfg:

```rust
fn main() {
    let out = std::path::PathBuf::from(std::env::var("OUT_DIR").unwrap());
    barracuda_event_router::bake_all(&out).unwrap();
    println!("cargo:rustc-cfg=rpc_schema_baked");
}
```

Register each request DTO in the wire crate:

```rust
barracuda_event_router::register!(SetLevelRequest);
```

`#[rpc_dynamic]` then embeds the baked schema beside the method. A runnable
version lives in `example-crates/schema-demo`.

## Examples

- `examples/components` — Component registration, typed RPCs, and lifecycle.
- `examples/events` — single-step Workflow loading, Events, and durable state.
- `examples/rpc_json` — `#[rpc_dynamic]` and `call_json` through the Event Router.
- `examples/workflow` — a multi-step Workflow with a `$previous.output` mapping link.
- `examples/persistence` — durable restore and `WorkflowClient::unload`.
- `examples/streaming` — a streaming Event into a streaming Workflow step.
- `examples/workflow_failure` — Method errors and `WorkflowInfo` failure counters.
- `example-crates/schema-demo` — the `build.rs` JSON Schema bake pipeline
  (`cargo run -p barracuda-event-router-schema-demo`).
- Lower-level examples under `crates/{rpc,router,workflow}/examples/`.

## Cardinality

Workflow edges support `Unary → Unary`, `Unary → Streaming`, and
`Streaming → Streaming`.
