//! Selects only the Platform implementation.

use std::{env, error::Error, fs, path::PathBuf};

use barracuda_platform_config::PlatformTarget;
use serde::Deserialize;

#[derive(Deserialize)]
struct PlatformIdentity {
    name: String,
    #[serde(rename = "crate")]
    crate_name: String,
    #[serde(rename = "type")]
    type_name: String,
}

fn main() -> Result<(), Box<dyn Error>> {
    println!("cargo:rerun-if-env-changed=BARRACUDA_PLATFORM");

    let manifest = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").ok_or("missing manifest dir")?);
    let root = manifest.join("../..");
    let target_os = env::var("CARGO_CFG_TARGET_OS")?;
    let target_arch = env::var("CARGO_CFG_TARGET_ARCH")?;
    let target = env::var("TARGET")?;
    let requested = env::var("BARRACUDA_PLATFORM").ok();
    let platform_name = barracuda_platform_config::resolve_platform(
        PlatformTarget::new(&target, &target_os, &target_arch),
        requested.as_deref(),
    )?;

    let platform_path = root
        .join("platforms")
        .join(&platform_name)
        .join("platform.yml");
    println!("cargo:rerun-if-changed={}", platform_path.display());
    let platform_yaml = fs::read_to_string(platform_path)?;
    let mut platforms = yaml_peg::serde::from_str::<PlatformIdentity>(&platform_yaml)?;
    if platforms.len() != 1 {
        return Err("Platform YAML must contain exactly one document".into());
    }
    let platform = platforms.pop().ok_or("Platform YAML is empty")?;
    if platform.name != platform_name {
        return Err("Platform YAML name does not match the selected Platform".into());
    }
    validate_rust_identifier(&platform.crate_name)?;
    validate_rust_identifier(&platform.type_name)?;

    let generated = format!(
        "/// Name of the independently selected Platform.\n\
         pub const PLATFORM_NAME: &str = {platform_name:?};\n\n\
         /// Independently selected Platform implementation.\n\
         pub type SelectedPlatform = ::{}::{};\n",
        platform.crate_name, platform.type_name
    );
    let output = PathBuf::from(env::var_os("OUT_DIR").ok_or("Cargo did not set OUT_DIR")?);
    fs::write(output.join("selected_platform.rs"), generated)?;
    Ok(())
}

fn validate_rust_identifier(value: &str) -> Result<(), Box<dyn Error>> {
    let mut bytes = value.bytes();
    let first = bytes.next().ok_or("empty Rust identifier")?;
    if !(first.is_ascii_alphabetic() || first == b'_')
        || !bytes.all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
    {
        return Err(format!("invalid Rust identifier `{value}`").into());
    }
    Ok(())
}
