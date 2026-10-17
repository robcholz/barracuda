//! Selects only the Platform implementation.

use std::{env, error::Error, fs, path::PathBuf};

fn main() -> Result<(), Box<dyn Error>> {
    let output = PathBuf::from(env::var_os("OUT_DIR").ok_or("Cargo did not set OUT_DIR")?);
    let manifest = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").ok_or("missing manifest dir")?);
    let root = manifest.join("../..");
    barracuda_platform_tool::sync(&root, true).map_err(|error| {
        format!(
            "{error}; Platform registries are source files, so run `cargo platform sync` before rebuilding"
        )
    })?;
    let selection_path = root.join(".barracuda/selected-platform");
    println!("cargo:rerun-if-changed={}", selection_path.display());
    let selected = fs::read_to_string(&selection_path).map_err(|error| {
        format!("no Platform selected; run `cargo board select` first: {error}")
    })?;
    let selected = selected.trim();
    let platform = barracuda_platform_config::discover_platforms(&root)?
        .into_iter()
        .find(|platform| platform.name() == selected)
        .ok_or_else(|| format!("selected Platform `{selected}` is not registered"))?;
    let platform_path = platform.directory().join("platform.yml");
    println!("cargo:rerun-if-changed={}", platform_path.display());
    let architecture = env::var("CARGO_CFG_TARGET_ARCH")
        .map_err(|error| format!("Cargo did not provide the target architecture: {error}"))?;

    let generated = format!(
        "/// Name of the independently selected Platform.\n\
         pub const PLATFORM_NAME: &str = {:?};\n\n\
         /// Fixed identity of the independently selected Platform.\n\
         pub const PLATFORM_INFO: ::barracuda_platform::PlatformInfo =\n\
             ::barracuda_platform::PlatformInfo::new({:?}, {:?}, {:?}, {:?});\n\n\
         /// Independently selected Platform implementation.\n\
         pub type SelectedPlatform = ::barracuda_platform_selection::{};\n\n\
         #[doc(hidden)]\n\
         pub use ::barracuda_platform_selection as __platform;\n\n\
         #[doc(hidden)]\n\
         #[macro_export]\n\
         macro_rules! platform_entry {{\n\
             ($($tokens:tt)*) => {{\n\
                 $crate::__platform::platform_entry!($($tokens)*);\n\
             }};\n\
         }}\n",
        platform.name(),
        platform.name(),
        platform.info().family(),
        architecture,
        platform.info().environment(),
        platform.type_name(),
    );
    fs::write(output.join("selected_platform.rs"), generated)?;
    Ok(())
}
