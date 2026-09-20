//! Bootstraps local selection packages before Cargo resolves the source workspace.

use std::{
    env,
    path::PathBuf,
    process::{Command, ExitCode},
};

#[allow(dead_code)]
#[path = "../../src/selection_packages.rs"]
mod selection_packages;

fn main() -> ExitCode {
    let workspace_root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..");
    if let Err(error) = selection_packages::ensure(&workspace_root) {
        eprintln!("error: failed to initialize local Board selection: {error}");
        return ExitCode::FAILURE;
    }

    let cargo = env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let status = Command::new(cargo)
        .current_dir(&workspace_root)
        .args([
            "run",
            "--config",
            "unstable.build-std=[\"std\"]",
            "--target-dir",
            "target/host-tools",
            "--target",
            "host-tuple",
            "--quiet",
            "--package",
            "barracuda-board-tool",
            "--",
        ])
        .args(env::args_os().skip(1))
        .status();

    match status {
        Ok(status) if status.success() => ExitCode::SUCCESS,
        Ok(status) => status
            .code()
            .and_then(|code| u8::try_from(code).ok())
            .map_or(ExitCode::FAILURE, ExitCode::from),
        Err(error) => {
            eprintln!("error: failed to start cargo board: {error}");
            ExitCode::FAILURE
        }
    }
}
