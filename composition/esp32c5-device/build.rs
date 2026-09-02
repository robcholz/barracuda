//! Final-link configuration for the ESP32-C5 firmware image.

use std::{env, fs, io, path::Path, process::Command};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let target = env::var("TARGET")?;
    if target != "riscv32imac-unknown-none-elf" || env::var_os("CARGO_FEATURE_FIRMWARE").is_none() {
        return Ok(());
    }

    let compiler_variable = format!("CC_{}", target.replace('-', "_"));
    println!("cargo:rerun-if-env-changed={compiler_variable}");
    println!("cargo:rerun-if-env-changed=CC");
    let compiler = env::var(&compiler_variable)
        .or_else(|_| env::var("CC"))
        .map_err(|_error| {
            io::Error::new(
                io::ErrorKind::NotFound,
                "ESP32-C5 firmware requires a RISC-V bare-metal C compiler",
            )
        })?;
    println!("cargo:rerun-if-changed=newlib_alloc_shim.c");

    // These libraries are deliberately selected by the final image rather
    // than by portable Lua. Rust/ESP archives appear first in the final link,
    // so ESP's allocator and ROM ABI satisfy their symbols; newlib only fills
    // the remaining ISO C surface required by the Lua VM.
    let mut libraries = Vec::new();
    for library in ["libc.a", "libm.a", "libnosys.a", "libgcc.a"] {
        let output = Command::new(&compiler)
            .args([
                "-march=rv32imac",
                "-mabi=ilp32",
                &format!("-print-file-name={library}"),
            ])
            .output()?;
        if !output.status.success() {
            return Err(io::Error::other("RISC-V C runtime query failed").into());
        }
        let path = std::str::from_utf8(&output.stdout)?.trim();
        let parent = Path::new(path)
            .parent()
            .ok_or_else(|| io::Error::other("RISC-V C runtime library has no parent"))?;
        println!("cargo:rustc-link-search=native={}", parent.display());
        libraries.push(path.to_owned());
    }

    // ESP already supplies these two symbols, but other newlib routines cause
    // their archive members to be selected as a side effect. Remove only the
    // colliding members from an image-local archive; the toolchain installation
    // remains untouched.
    let out_dir = env::var("OUT_DIR")?;
    let allocation_shim = Path::new(&out_dir).join("newlib_alloc_shim.o");
    let shim_status = Command::new(&compiler)
        .args([
            "-march=rv32imac",
            "-mabi=ilp32",
            "-ffunction-sections",
            "-fdata-sections",
            "-c",
            "newlib_alloc_shim.c",
            "-o",
            allocation_shim
                .to_str()
                .ok_or_else(|| io::Error::other("OUT_DIR is not UTF-8"))?,
        ])
        .status()?;
    if !shim_status.success() {
        return Err(io::Error::other("compiling newlib allocation adapter failed").into());
    }

    let filtered_libc = Path::new(&out_dir).join("libc-barracuda.a");
    let libc = libraries
        .first()
        .ok_or_else(|| io::Error::other("RISC-V libc was not resolved"))?;
    fs::copy(libc, &filtered_libc)?;
    let archiver_variable = format!("AR_{}", target.replace('-', "_"));
    println!("cargo:rerun-if-env-changed={archiver_variable}");
    println!("cargo:rerun-if-env-changed=AR");
    let archiver = env::var(&archiver_variable)
        .or_else(|_| env::var("AR"))
        .unwrap_or_else(|_| compiler.replace("gcc", "ar"));
    let status = Command::new(archiver)
        .arg("d")
        .arg(&filtered_libc)
        .args(["libc_a-sprintf.o", "libc_a-stack_protector.o"])
        .status()?;
    if !status.success() {
        return Err(io::Error::other("filtering newlib archive failed").into());
    }
    let first_library = libraries
        .first_mut()
        .ok_or_else(|| io::Error::other("RISC-V libc was not resolved"))?;
    *first_library = filtered_libc
        .to_str()
        .ok_or_else(|| io::Error::other("filtered libc path is not UTF-8"))?
        .to_owned();

    // Native-library directives are placed before Rust archives by rustc.
    // Explicit linker arguments stay at the end, after ESP has supplied its
    // ABI, and therefore pull only still-unresolved functions from newlib.
    println!("cargo:rustc-link-arg={}", allocation_shim.display());
    println!("cargo:rustc-link-arg=--start-group");
    for library in libraries {
        println!("cargo:rustc-link-arg={library}");
    }
    println!("cargo:rustc-link-arg=--end-group");
    Ok(())
}
