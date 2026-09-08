//! Supplies the Browser execution environment's checked-in WASI C runtime.

use std::{env, path::PathBuf};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    if env::var("CARGO_CFG_TARGET_ARCH").as_deref() != Ok("wasm32") {
        return Ok(());
    }

    let root = PathBuf::from(env::var("CARGO_MANIFEST_DIR")?).join("vendor/wasi");
    for archive in ["libc.a", "libwasi-emulated-signal.a", "libsetjmp.a"] {
        println!("cargo::rerun-if-changed={}", root.join(archive).display());
    }
    println!("cargo::rustc-link-search=native={}", root.display());
    println!("cargo::rustc-link-lib=static=c");
    println!("cargo::rustc-link-lib=static=wasi-emulated-signal");
    println!("cargo::rustc-link-lib=static=setjmp");
    Ok(())
}
