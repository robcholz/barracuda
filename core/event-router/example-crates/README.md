# Event Router example crates

Standalone example crates that need their own `Cargo.toml`, `build.rs`, or
feature wiring — things a single-file `core/event-router/examples/*.rs` target
cannot express.

| Example | What it demonstrates |
| --- | --- |
| `schema-demo` | The host-side JSON Schema bake pipeline: a `build.rs` runs `bake_all` over a `schema-wire` request DTO, and `#[rpc_dynamic]` embeds the baked schema beside the method |

Run it from the workspace root:

```console
cargo run -p barracuda-event-router-schema-demo
```
