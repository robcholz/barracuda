# barracuda-rpc-schema

Host-side JSON Schema bake pipeline for `#[rpc_json]` RPC methods. `schemars`
runs on the host (from a `build.rs`); only the resulting `&'static str` is baked
into the firmware, which stays `no_std` and schemars-free.

This crate is internal. Consumers reach it through the facade:
`barracuda_event_router::{register, bake_all, SchemaEntry}`, enabled with the
facade's `schema` feature (host-only). The examples below use the facade path.

## Why a `build.rs` needs three crates

A `build.rs` cannot run `schemars` on types defined in **its own crate**, so the
request DTOs must live in a crate the bake `build.rs` can build-depend on. The
pieces:

| Crate | Holds | schemars |
| --- | --- | --- |
| **wire** | request DTOs; `#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]` + `register!(RequestType)` (feature-gated) | optional dep behind a `schema` feature |
| **methods** | `impl RpcMethod` + `#[rpc_json]`; the bake `build.rs`; build-depends on `wire` (`features = ["schema"]`) and this crate | none on the device |
| **barracuda-rpc-schema** (this crate) | `SchemaEntry`, `register!`, `bake_all` | yes (host lib) |

## Wiring

**wire crate** — register each request type (host-only):

```rust
#![cfg_attr(not(feature = "schema"), no_std)]

#[repr(C)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(serde::Serialize, serde::Deserialize /* + zerocopy derives */)]
pub struct SetLevelRequest { pub session: u32, pub level: u32 }

#[cfg(feature = "schema")]
barracuda_event_router::register!(SetLevelRequest);
```

**methods crate `build.rs`** — a thin shim, no baking logic:

```rust
// Force the wire crate to link so its `register!` submissions are visible.
use my_wire_crate as _;

fn main() {
    let out = std::env::var_os("OUT_DIR").unwrap();
    barracuda_event_router::bake_all(std::path::Path::new(&out)).unwrap();
    println!("cargo:rustc-cfg=rpc_schema_baked");
}
```

`#[rpc_json]` then embeds `$OUT_DIR/<Request>.json` via `include_str!` when
`rpc_schema_baked` is set, and `RpcMethod::schema()` returns that `&'static str`.
Without the pipeline the method stays JSON-callable and `schema()` is `None`.

Because `schemars` derives the schema from the Rust type (honoring `serde`
attributes), the baked schema cannot drift from what `call_json` accepts.
