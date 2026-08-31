//! Selects only the Board and its Board HAL.

use std::{env, error::Error, fs, path::PathBuf};

use barracuda_board_config::{
    parse, read_selected_board, render_rust, BoardDefinition, PeripheralParameter,
    SELECTED_BOARD_PATH,
};

fn main() -> Result<(), Box<dyn Error>> {
    let manifest = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").ok_or("missing manifest dir")?);
    let root = manifest.join("../..");
    let selection_path = root.join(SELECTED_BOARD_PATH);
    println!("cargo:rerun-if-changed={}", selection_path.display());
    let board_name =
        read_selected_board(&root)?.ok_or("no Board selected; run `cargo board select` first")?;
    let board_path = root
        .join("boards/configs")
        .join(&board_name)
        .join("board.yml");
    println!("cargo:rerun-if-changed={}", board_path.display());
    let board = parse(&fs::read_to_string(board_path)?)?;
    if board.name() != board_name {
        return Err("Board bundle directory and Board name differ".into());
    }

    let hal_type = match board_name.as_str() {
        "stm32f429zi-nucleo" => {
            validate_stm32f429zi_nucleo_adapter(&board)?;
            "::barracuda_board_stm32f429zi_nucleo::Stm32f429ziNucleoHal"
        }
        _ if board.has_hardware_surface() => {
            return Err(format!(
                "Board `{board_name}` declares hardware resources but has no registered Board HAL adapter"
            )
            .into());
        }
        _ => "::barracuda_board_hal::EmptyBoardHal",
    };
    let mut generated = render_rust(&board);
    generated.push_str(&format!(
        "\n/// Board HAL selected independently from Platform.\n\
         pub type SelectedBoardHal = {hal_type};\n"
    ));
    let output = PathBuf::from(env::var_os("OUT_DIR").ok_or("Cargo did not set OUT_DIR")?);
    fs::write(output.join("selected_board.rs"), generated)?;
    Ok(())
}

fn validate_stm32f429zi_nucleo_adapter(board: &BoardDefinition) -> Result<(), Box<dyn Error>> {
    let status_led = board
        .builtin_peripheral("status-led")
        .ok_or("STM32F429ZI Nucleo adapter requires built-in `status-led`")?;
    let user_button = board
        .exposed_io()
        .gpio("user-button")
        .ok_or("STM32F429ZI Nucleo adapter requires exposed GPIO `user-button`")?;
    let active_high = PeripheralParameter::String("high".into());

    if board.builtin_peripheral_count() != 1
        || status_led.driver() != "indicator-led"
        || status_led.binding("pin") != Some("PB0")
        || status_led.parameter("active-level") != Some(&active_high)
        || board.exposed_io().len() != 1
        || user_button.pin() != "PC13"
    {
        return Err(
            "STM32F429ZI Nucleo board.yml does not match its concrete Board HAL adapter".into(),
        );
    }
    Ok(())
}
