//! Bakes every `register!`-submitted request schema into `OUT_DIR`.

// Force the wire crate to link so its `register!` inventory submission is
// visible to `bake_all`.
use schema_wire as _;

fn main() {
    let out = std::env::var_os("OUT_DIR").expect("Cargo sets OUT_DIR for build scripts");
    barracuda_event_router::bake_all(std::path::Path::new(&out)).expect("bake JSON schemas");
    println!("cargo:rustc-cfg=rpc_schema_baked");
}
