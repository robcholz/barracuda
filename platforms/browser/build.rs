//! Links the Browser execution environment's WASI C runtime support.

use std::{env, path::PathBuf};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("wasi") {
        return Ok(());
    }

    println!("cargo::rerun-if-env-changed=WASI_SDK_PATH");
    let sdk = PathBuf::from(
        env::var_os("WASI_SDK_PATH")
            .ok_or("Browser builds require WASI_SDK_PATH to point to an installed WASI SDK")?,
    );
    let root = sdk.join("share/wasi-sysroot/lib/wasm32-wasip1");
    println!("cargo::rustc-link-search=native={}", root.display());
    println!("cargo::rustc-link-lib=static=wasi-emulated-signal");
    println!("cargo::rustc-link-lib=static=setjmp");
    Ok(())
}
