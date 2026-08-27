//! `cargo plugin` workspace maintenance commands.

use std::{env, path::PathBuf, process::ExitCode};

use barracuda_plugin_tool::sync;

fn main() -> ExitCode {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let arguments = env::args().skip(1).collect::<Vec<_>>();
    let result = match arguments.as_slice() {
        [command] if command == "sync" => sync(&root, false),
        [command, option] if command == "sync" && option == "--check" => sync(&root, true),
        _ => Err("usage: cargo plugin sync [--check]".into()),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("error: {error}");
            ExitCode::FAILURE
        }
    }
}
