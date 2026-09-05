//! Selects only the Platform implementation.

use std::{env, error::Error, fs, path::PathBuf};

use barracuda_platform_config::PlatformTarget;

fn main() -> Result<(), Box<dyn Error>> {
    println!("cargo:rerun-if-env-changed=BARRACUDA_PLATFORM");
    println!("cargo:rerun-if-env-changed=BARRACUDA_GENERATED_BUILD");

    let output = PathBuf::from(env::var_os("OUT_DIR").ok_or("Cargo did not set OUT_DIR")?);
    if env::var("BARRACUDA_GENERATED_BUILD").as_deref() != Ok("1") {
        fs::write(
            output.join("selected_platform.rs"),
            "/// No Platform is injected into the source workspace.\n\
             pub const PLATFORM_NAME: &str = \"unconfigured\";\n\n\
             /// Placeholder used only while compiling workspace tooling.\n\
             pub type SelectedPlatform = super::UnconfiguredPlatform;\n",
        )?;
        return Ok(());
    }

    let manifest = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").ok_or("missing manifest dir")?);
    let root = manifest.join("../..");
    let target_os = env::var("CARGO_CFG_TARGET_OS")?;
    let target_arch = env::var("CARGO_CFG_TARGET_ARCH")?;
    let target = env::var("TARGET")?;
    let requested = env::var("BARRACUDA_PLATFORM").ok();
    let platform = barracuda_platform_config::resolve_platform(
        &root,
        PlatformTarget::new(&target, &target_os, &target_arch),
        requested.as_deref(),
    )?;
    let platform_path = platform.directory().join("platform.yml");
    println!("cargo:rerun-if-changed={}", platform_path.display());

    let generated = format!(
        "/// Name of the independently selected Platform.\n\
         pub const PLATFORM_NAME: &str = {:?};\n\n\
         /// Independently selected Platform implementation.\n\
         pub type SelectedPlatform = ::barracuda_selected_platform_implementation::{};\n",
        platform.name(),
        platform.type_name()
    );
    fs::write(output.join("selected_platform.rs"), generated)?;
    Ok(())
}
