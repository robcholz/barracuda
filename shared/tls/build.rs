//! Compiles the C library functions mbedTLS calls on bare-metal targets.

use std::{env, error::Error, path::PathBuf};

/// The copied musl sources and Barracuda's wrappers, relative to `c/`.
const SOURCES: [&str; 4] = [
    "musl/memchr.c",
    "musl/strcmp.c",
    "musl/vfprintf.c",
    "runtime.c",
];

fn main() -> Result<(), Box<dyn Error>> {
    let manifest = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").ok_or("missing manifest dir")?);
    let c = manifest.join("c");
    println!("cargo:rerun-if-changed={}", c.display());

    // Host targets link their system C library.
    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("none") {
        return Ok(());
    }
    cc::Build::new()
        .std("c11")
        // musl is written for its own warning flags, not -Wall -Wextra.
        .warnings(false)
        .extra_warnings(false)
        // These headers come before the compiler's own.
        .include(c.join("include"))
        .include(c.join("musl"))
        .flag("-include")
        .flag(c.join("weak.h").to_string_lossy().as_ref())
        .files(SOURCES.map(|name| c.join(name)))
        .try_compile("tls_c_runtime")?;
    Ok(())
}
