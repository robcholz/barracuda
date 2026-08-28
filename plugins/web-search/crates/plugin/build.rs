//! Bakes Web Search dynamic RPC schemas.

use barracuda_web_search_wire as _;

fn main() -> std::io::Result<()> {
    let out = std::env::var_os("OUT_DIR")
        .ok_or_else(|| std::io::Error::other("Cargo did not set OUT_DIR"))?;
    barracuda_event_router::bake_all(std::path::Path::new(&out))?;
    println!("cargo:rustc-cfg=rpc_schema_baked");
    Ok(())
}
