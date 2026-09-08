use std::{env, path::PathBuf};

fn main() {
    const WASM_VENDOR: &str = "vendor/wasm32-wasip1";
    for archive in [
        "liblua5.4.a",
        "libc.a",
        "libwasi-emulated-signal.a",
        "libsetjmp.a",
    ] {
        println!("cargo::rerun-if-changed={WASM_VENDOR}/{archive}");
    }

    if env::var("CARGO_CFG_TARGET_ARCH").as_deref() == Ok("wasm32")
        && env::var_os("CARGO_FEATURE_WASM_STATIC").is_some()
    {
        let manifest = PathBuf::from(
            env::var_os("CARGO_MANIFEST_DIR").expect("Cargo provides CARGO_MANIFEST_DIR"),
        );
        println!(
            "cargo::rustc-link-search=native={}",
            manifest.join(WASM_VENDOR).display()
        );
        println!("cargo::rustc-link-lib=static=lua5.4");
        println!("cargo::rustc-link-lib=static=c");
        println!("cargo::rustc-link-lib=static=wasi-emulated-signal");
        println!("cargo::rustc-link-lib=static=setjmp");
    }
}
