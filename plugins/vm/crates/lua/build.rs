//! Makes the existing `vendored` Lua feature work for native and WebAssembly targets.

#[cfg(feature = "vendored")]
use std::{env, path::PathBuf};

fn main() {
    #[cfg(feature = "vendored")]
    build_vendored();
}

#[cfg(feature = "vendored")]
fn build_vendored() {
    if env::var("CARGO_CFG_TARGET_ARCH").as_deref() == Ok("wasm32") {
        link_wasi_artifacts();
    } else {
        lunka_src::Build::for_current()
            .add_lunka_src()
            .compile("lua");
    }
}

#[cfg(feature = "vendored")]
fn link_wasi_artifacts() {
    const WASI_VENDOR: &str = "vendor/wasm32-wasip1";
    let manifest = PathBuf::from(
        env::var_os("CARGO_MANIFEST_DIR").expect("Cargo provides CARGO_MANIFEST_DIR"),
    );
    println!("cargo::rerun-if-changed={WASI_VENDOR}/liblua5.4.a");
    println!(
        "cargo::rustc-link-search=native={}",
        manifest.join(WASI_VENDOR).display()
    );
    println!("cargo::rustc-link-lib=static=lua5.4");
}
