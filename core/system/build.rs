//! Revalidates the generated System Plugin registry when `plugins/` changes.

use std::{env, error::Error, path::PathBuf};

fn main() -> Result<(), Box<dyn Error>> {
    let manifest = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").ok_or("missing manifest dir")?);
    let root = manifest.join("../..");
    println!("cargo:rerun-if-changed={}", root.join("plugins").display());
    println!("cargo:rerun-if-changed=Cargo.toml");
    println!("cargo:rerun-if-changed=src/lib.rs");
    barracuda_plugin_tool::sync(&root, true).map_err(|error| {
        format!("{error}; Plugin registries are source files, so run `cargo plugin sync` before rebuilding").into()
    })
}
