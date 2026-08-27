//! `cargo board` entry point.

use std::{io, path::PathBuf, process::ExitCode};

use barracuda_board_tool::{execute, Cli};
use clap::Parser;

fn main() -> ExitCode {
    let workspace_root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let mut output = io::stdout().lock();
    match execute(Cli::parse(), &workspace_root, &mut output) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("error: {error}");
            ExitCode::FAILURE
        }
    }
}
