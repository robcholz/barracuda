//! Selects only the Board and its Board HAL.

use std::{env, error::Error, fs, path::PathBuf};

use barracuda_board_config::{parse, render_rust};

fn main() -> Result<(), Box<dyn Error>> {
    println!("cargo:rerun-if-env-changed=BARRACUDA_BOARD");

    let manifest = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").ok_or("missing manifest dir")?);
    let root = manifest.join("../..");
    let target_os = env::var("CARGO_CFG_TARGET_OS")?;
    let target_arch = env::var("CARGO_CFG_TARGET_ARCH")?;
    let default = default_board(&target_os, &target_arch)?;
    let board_name = env::var("BARRACUDA_BOARD").unwrap_or_else(|_| default.into());
    validate_name(&board_name)?;
    let board_path = root
        .join("boards/configs")
        .join(&board_name)
        .join("board.yml");
    println!("cargo:rerun-if-changed={}", board_path.display());
    let board = parse(&fs::read_to_string(board_path)?)?;
    if board.name() != board_name {
        return Err("Board bundle directory and Board name differ".into());
    }

    // The current Board schema declares no peripheral matrix yet. Therefore
    // every current Board has an explicitly empty Board HAL, independent of
    // which Platform is selected. When a Board gains peripherals, its generated
    // HAL type will be selected from those Board declarations rather than from
    // a Platform name or family.
    let hal_type = "::barracuda_board_hal::EmptyBoardHal";
    let mut generated = render_rust(&board);
    generated.push_str(&format!(
        "\n/// Board HAL selected independently from Platform.\n\
         pub type SelectedBoardHal = {hal_type};\n"
    ));
    let output = PathBuf::from(env::var_os("OUT_DIR").ok_or("Cargo did not set OUT_DIR")?);
    fs::write(output.join("selected_board.rs"), generated)?;
    Ok(())
}

fn default_board(target_os: &str, target_arch: &str) -> Result<&'static str, Box<dyn Error>> {
    match (target_os, target_arch) {
        ("macos", _) => Ok("local-macos"),
        ("linux", _) => Ok("local-linux"),
        (_, "riscv32") => Ok("esp32c6-devkitc-1"),
        (_, "arm") => Ok("stm32f429zi-nucleo"),
        _ => Err(format!(
            "no default Barracuda Board is registered for OS `{target_os}` and architecture `{target_arch}`"
        )
        .into()),
    }
}

fn validate_name(value: &str) -> Result<(), Box<dyn Error>> {
    if value.is_empty()
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err(format!("invalid Board name `{value}`").into());
    }
    Ok(())
}
