use std::env;
use std::path::PathBuf;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    if env::var("CARGO_CFG_TARGET_ARCH").as_deref() == Ok("wasm32") {
        return link_wasi_artifact();
    }

    build_native()
}

fn link_wasi_artifact() -> Result<(), Box<dyn std::error::Error>> {
    if !cfg!(feature = "multiversion")
        || cfg!(feature = "assertions")
        || cfg!(feature = "malloc")
        || cfg!(feature = "software-intrinsics")
        || cfg!(feature = "trace")
        || cfg!(feature = "unstable-littlefs-patched")
    {
        return Err("the checked-in WASI artifact supports Barracuda's multiversion, no-malloc feature set only".into());
    }

    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR")?);
    let artifact_dir = manifest_dir.join("vendor/wasm32-wasip1");
    println!("cargo::rerun-if-changed={}", artifact_dir.display());
    println!("cargo::rustc-link-search=native={}", artifact_dir.display());
    println!("cargo::rustc-link-lib=static=lfs-sys");
    Ok(())
}

#[cfg(not(target_os = "wasi"))]
fn build_native() -> Result<(), Box<dyn std::error::Error>> {
    let littlefs_path = if cfg!(feature = "unstable-littlefs-patched") {
        "littlefs-patched"
    } else {
        "littlefs"
    };
    let out_path = PathBuf::from(env::var("OUT_DIR")?);
    let lfs_h = std::fs::read_to_string(format!("{littlefs_path}/lfs.h"))?;
    println!("cargo::rerun-if-changed={littlefs_path}/lfs.h");
    println!("cargo::rerun-if-changed={littlefs_path}/lfs.c");
    println!("cargo::rerun-if-changed={littlefs_path}/lfs_util.c");
    let out_lfs_h = out_path.join("lfs.h");
    std::fs::write(
        &out_lfs_h,
        lfs_h.replace(
            "#include \"lfs_util.h\"",
            "#include <stdint.h>\n#include <stdbool.h>",
        ),
    )?;

    let mut c = cc::Build::new();
    c.flag("-std=c99")
        .flag("-DLFS_NO_DEBUG")
        .flag("-DLFS_NO_WARN")
        .flag("-DLFS_NO_ERROR")
        .include(&out_path)
        .include(littlefs_path)
        .file(format!("{littlefs_path}/lfs.c"))
        .file(format!("{littlefs_path}/lfs_util.c"));
    if cfg!(feature = "software-intrinsics") {
        c.flag("-DLFS_NO_INTRINSICS");
    }
    if !cfg!(feature = "assertions") {
        c.flag("-DLFS_NO_ASSERT");
    }
    if cfg!(feature = "trace") {
        c.flag("-DLFS_YES_TRACE");
    }
    if !cfg!(feature = "malloc") {
        c.flag("-DLFS_NO_MALLOC");
    }
    if cfg!(feature = "multiversion") {
        c.flag("-DLFS_MULTIVERSION");
    }
    c.compile("lfs-sys");

    let mut bindings = bindgen::Builder::default()
        .header(out_lfs_h.to_string_lossy())
        .clang_arg("-std=c99")
        .clang_arg("-DLFS_NO_DEBUG")
        .clang_arg("-DLFS_NO_WARN")
        .clang_arg("-DLFS_NO_ERROR");
    if cfg!(feature = "multiversion") {
        bindings = bindings.clang_arg("-DLFS_MULTIVERSION");
    }
    bindings
        .derive_default(true)
        .use_core()
        .allowlist_item("lfs_.*")
        .allowlist_item("LFS_.*")
        .generate()?
        .write_to_file(out_path.join("bindings.rs"))?;
    Ok(())
}

#[cfg(target_os = "wasi")]
fn build_native() -> Result<(), Box<dyn std::error::Error>> {
    unreachable!()
}
