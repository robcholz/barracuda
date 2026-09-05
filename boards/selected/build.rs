//! Selects only the Board and its Board HAL.

use std::{env, error::Error, fs, path::PathBuf};

use barracuda_board_config::{parse, read_selected_board, render_rust, SELECTED_BOARD_PATH};

fn main() -> Result<(), Box<dyn Error>> {
    println!("cargo:rerun-if-env-changed=BARRACUDA_GENERATED_BUILD");
    let output = PathBuf::from(env::var_os("OUT_DIR").ok_or("Cargo did not set OUT_DIR")?);
    let manifest = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").ok_or("missing manifest dir")?);
    let root = manifest.join("../..");
    barracuda_board_tool::sync(&root, true).map_err(|error| {
        format!(
            "{error}; Board registries are source files, so run `cargo board sync` before rebuilding"
        )
    })?;
    if env::var("BARRACUDA_GENERATED_BUILD").as_deref() != Ok("1") {
        fs::write(
            output.join("selected_board.rs"),
            "/// Placeholder Board used only while compiling workspace tooling.\n\
             pub const BOARD: ::barracuda_board::Board = ::barracuda_board::Board::new(\n\
                 \"unconfigured\",\n\
                 ::barracuda_board::Hardware::new(\"unconfigured\"),\n\
                 ::barracuda_board::NativeLayout::new(\"unconfigured\"),\n\
             );\n\n\
             /// Empty placeholder used outside a generated target build.\n\
             pub type SelectedBoardHal = ::barracuda_board_hal::EmptyBoardHal;\n",
        )?;
        return Ok(());
    }

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

    let hal_type = match board.board_hal() {
        Some(board_hal) => format!(
            "::barracuda_selected_board_hal_implementation::{}",
            board_hal.type_name()
        ),
        None if board.has_hardware_surface() => {
            return Err(format!(
                "Board `{board_name}` declares hardware resources but has no `board-hal` dependency"
            )
            .into());
        }
        None => String::from("::barracuda_board_hal::EmptyBoardHal"),
    };
    let mut generated = render_rust(&board);
    generated.push_str(&format!(
        "\n/// Board HAL selected independently from Platform.\n\
         pub type SelectedBoardHal = {hal_type};\n"
    ));
    fs::write(output.join("selected_board.rs"), generated)?;
    Ok(())
}
