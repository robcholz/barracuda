//! Selects only the Board and its Board HAL.

use std::{env, error::Error, fs, path::PathBuf};

use barracuda_board_config::{parse, read_selected_board, render_rust, SELECTED_BOARD_PATH};
use barracuda_driver_config::{load_catalog, render_board_hal, resolve_board};

fn main() -> Result<(), Box<dyn Error>> {
    let output = PathBuf::from(env::var_os("OUT_DIR").ok_or("Cargo did not set OUT_DIR")?);
    let manifest = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").ok_or("missing manifest dir")?);
    let root = manifest.join("../..");
    barracuda_board_tool::sync(&root, true).map_err(|error| {
        format!(
            "{error}; Board registries are source files, so run `cargo board sync` before rebuilding"
        )
    })?;
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

    let catalog = load_catalog(&root)?;
    let resolved = resolve_board(&board, &catalog)?;
    let mut generated = render_rust(&board);
    generated.push('\n');
    generated.push_str(&render_board_hal(&board, &resolved)?);
    fs::write(output.join("selected_board.rs"), generated)?;
    Ok(())
}
