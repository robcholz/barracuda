//! Selects only the Board and its Board HAL.

use std::{env, error::Error, fs, path::PathBuf};

use barracuda_board_config::{parse, read_selected_board, render_rust, SELECTED_BOARD_PATH};

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
