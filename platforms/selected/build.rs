//! Selects only the Platform implementation.

use std::{env, error::Error, fs, path::PathBuf};

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
    let default = default_platform(&target_os, &target_arch)?;
    let platform_name = env::var("BARRACUDA_PLATFORM").unwrap_or_else(|_| default.into());
    validate_name(&platform_name)?;
    validate_target(&platform_name, &target_os, &target_arch)?;

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

fn default_platform(target_os: &str, target_arch: &str) -> Result<&'static str, Box<dyn Error>> {
    match (target_os, target_arch) {
        ("macos", _) => Ok("macos"),
        ("linux", _) => Ok("linux"),
        (_, "riscv32") => Ok("esp32c6"),
        (_, "arm") => Ok("stm32"),
        _ => Err(format!(
            "no Barracuda Platform is registered for OS `{target_os}` and architecture `{target_arch}`"
        )
        .into()),
    }
}

fn validate_target(name: &str, target_os: &str, target_arch: &str) -> Result<(), Box<dyn Error>> {
    let valid = match name {
        "macos" => target_os == "macos",
        "linux" => target_os == "linux",
        "esp32c6" => target_arch == "riscv32",
        "stm32" => target_arch == "arm",
        _ => false,
    };
    if valid {
        Ok(())
    } else {
        Err(format!(
            "Platform `{name}` cannot be built for OS `{target_os}` and architecture `{target_arch}`"
        )
        .into())
    }
}

fn validate_name(value: &str) -> Result<(), Box<dyn Error>> {
    if value.is_empty()
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err(format!("invalid Platform name `{value}`").into());
    }
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
