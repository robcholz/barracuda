//! Selects independent Board and Platform YAML documents for the target facade.

use std::{env, error::Error, fs, path::PathBuf};

use barracuda_board_config::{parse, render_rust};
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
    println!("cargo:rerun-if-env-changed=BARRACUDA_BOARD");
    println!("cargo:rerun-if-env-changed=BARRACUDA_PLATFORM");

    let manifest = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").ok_or("missing manifest dir")?);
    let root = manifest.join("../..");
    let board_name = env::var("BARRACUDA_BOARD").unwrap_or_else(|_| "local-host".into());
    validate_file_stem(&board_name)?;
    let board_path = root
        .join("boards/configs")
        .join(&board_name)
        .join("board.yml");
    println!("cargo:rerun-if-changed={}", board_path.display());
    let board = parse(&fs::read_to_string(&board_path)?)?;
    if board.name() != board_name {
        return Err(format!(
            "Board name `{}` does not match `{board_name}`",
            board.name()
        )
        .into());
    }

    let platform_name = env::var("BARRACUDA_PLATFORM").unwrap_or_else(|_| "host".into());
    validate_file_stem(&platform_name)?;
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

    let mut generated = render_rust(&board);
    generated.push_str(&format!(
        "\n/// Platform selected by the build configuration.\n\
         pub type SelectedPlatform = ::{}::{};\n",
        platform.crate_name, platform.type_name
    ));
    let output = PathBuf::from(env::var_os("OUT_DIR").ok_or("Cargo did not set OUT_DIR")?);
    fs::write(output.join("selected_target.rs"), generated)?;
    Ok(())
}

fn validate_file_stem(value: &str) -> Result<(), Box<dyn Error>> {
    if value.is_empty()
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err(format!("invalid configuration name `{value}`").into());
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
