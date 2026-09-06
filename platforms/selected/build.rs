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

    let application_entry = if let Some(entry) = platform.application().entry() {
        let path = platform.directory().join(entry.source());
        println!("cargo:rerun-if-changed={}", path.display());
        if !path.is_file() {
            return Err(format!(
                "selected Platform `{selected}` application entry does not exist: {}",
                path.display()
            )
            .into());
        }
        format!("include!({path:?});")
    } else {
        format!(
            "compile_error!({:?});",
            format!(
                "selected Platform `{selected}` cannot run barracuda-system until its bundle declares `application.entry.source`"
            )
        )
    };

    let generated = format!(
        "/// Name of the independently selected Platform.\n\
         pub const PLATFORM_NAME: &str = {:?};\n\n\
         /// Independently selected Platform implementation.\n\
         pub type SelectedPlatform = ::{}::{};\n\n\
         #[doc(hidden)]\n\
         #[macro_export]\n\
         macro_rules! application_entry {{\n\
             () => {{ {} }};\n\
         }}\n",
        platform.name(),
        platform.crate_name(),
        platform.type_name(),
        application_entry
    );
    fs::write(output.join("selected_platform.rs"), generated)?;
    Ok(())
}
