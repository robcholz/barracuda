# Event Router Usage

This guide is for callers: component authors who register RPC methods, emit
events, and load workflows. For the architecture and rationale, see
[`design.md`](design.md).

## Quick start

```rust
let lanes = Box::leak(Box::new(RpcLaneStorage::<N, M, Q>::new()));
let filesystem = Box::leak(Box::new(MemFs::new()));
let mut router = EventRouter::new(lanes, filesystem, "workflows")?;
router.load(Box::new(MyComponent))?;
// drive `router` from the cooperative executor
```

`N` is the lane count, `M` the frame size in bytes, and `Q` the waiter count.

## 1. Define a message DTO

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

The serde derives define the JSON shape; the zerocopy derives define the wire
bytes; `RpcWire` generates the field table used by mapping links (its field
names follow serde, so `rename`/`rename_all` are honored).

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

`Input`/`Output` are `Unary` or `Streaming`. Add `#[rpc_dynamic]` when the
method should be callable through JSON and addressable by field name in
workflow mapping links.

## 3. Register a component

```rust
impl Component<M> for MyComponent {
    fn register(&mut self, context: &mut RegisterContext<'_, M>) -> ComponentResult<()> {
        context.register_rpc::<SetLevel, _>(|_context, request: RpcFrame<SetLevelRequest>| async move {
            // return Ok(Ok(())) or Ok(Err(SetLevelError::...))
            Ok(Ok(()))
        })?;
        Ok(())
    }
    // run / unregister as needed
}
```

Handlers receive `RpcContext` (call lineage, nested client) and an
`RpcFrame<Request>`; stream inputs use `RpcStream<RpcFrame<Request>>`.

## 4. Call RPCs

```rust
// Typed
let outcome = client.call::<SetLevel>(SetLevelRequest { session: 7, level: 2 })?.await?;

// JSON (requires #[rpc_dynamic])
let value = client
    .call_json(&RpcAddress::try_from(SetLevel::ADDRESS)?, &serde_json::json!({
        "session": 7, "level": 2
    }))
    .await?;
// -> {"ok": true, "value": ...} or {"ok": false, "error": ...}

// Raw wire
let (writer, reader) = client.call_payload(&address)?;
```

## 5. Events and workflows

### Emit an event

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

### Load a workflow

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

Step 0 is the ingress: the event payload becomes its request. Later steps are
linked by `arguments`:

| `arguments` | Link |
| --- | --- |
| absent | `Direct` — previous response passed byte-for-byte (types must match). |
| present, no `$` | `Literal` — request built from the literal arguments alone. |
| present, with `$` | `Mapping` — literal arguments plus wire-copied fields. |

References use `$previous.output.<field>` with a single top-level serde field
name. In a mapping, the destination DTO field written by a `$` reference should
carry `#[serde(default)]` (or a placeholder in the literal arguments), because
the literal JSON does not include it.

### Unload

```rust
client.unload(&WorkflowId::try_from("demo")?).await?;
```

## 6. Bake JSON Schemas

Add a `build.rs` that runs the host-side bake and enables the cfg:

```rust
fn main() {
    let out = std::path::PathBuf::from(std::env::var("OUT_DIR").unwrap());
    barracuda_rpc_schema::bake_all(&out).unwrap();
    println!("cargo:rustc-cfg=rpc_schema_baked");
}
```

Register each request DTO:

```rust
barracuda_rpc_schema::register!(SetLevelRequest);
```

The baked schema is then available through the method's `Dynamic` surface
(`#[rpc_dynamic]`). Enable the facade re-export with the `schema` feature.

## Examples

- `examples/components` — Component registration, typed RPCs, and lifecycle.
- `examples/events` — single-step Workflow loading, events, and durable state.
- `examples/rpc_json` — `#[rpc_dynamic]` and `call_json` through the Event Router.
- `examples/workflow` — a multi-step Workflow with a `$previous.output` mapping link.
- `examples/persistence` — durable restore and `WorkflowClient::unload`.
- `examples/streaming` — a streaming Event into a streaming Workflow step.
- `examples/workflow_failure` — Method errors and `WorkflowInfo` failure counters.
- `example-crates/schema-demo` — the `build.rs` JSON Schema bake pipeline
  (`cargo run -p barracuda-event-router-schema-demo`).
- Lower-level examples under `crates/{rpc,router,workflow}/examples/`.

## Cardinality rules

Workflow edges support `Unary → Unary`, `Unary → Streaming`, and
`Streaming → Streaming`. `Streaming → Unary` is rejected.
