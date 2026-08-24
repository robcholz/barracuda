//! `cargo board` entry point.

use std::{env, io, path::PathBuf, process::ExitCode};

fn main() -> ExitCode {
    let workspace_root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let mut output = io::stdout().lock();
    match barracuda_board_tool::run(env::args().skip(1), &workspace_root, &mut output) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("error: {error}");
            ExitCode::FAILURE
        }
    }
}
