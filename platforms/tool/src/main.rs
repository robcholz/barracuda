//! `cargo platform` workspace maintenance commands.

use std::{env, ffi::OsString, path::PathBuf, process::ExitCode};

use anstream::{eprintln, println};
use barracuda_cli_style::{DIM, EMPHASIS, ERROR, SUCCESS};
use barracuda_platform_tool::{launch, sync_with_report, SyncStatus};
use clap::{Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(
    name = "cargo platform",
    bin_name = "cargo platform",
    version,
    about,
    styles = barracuda_cli_style::CLI_STYLES
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Synchronize discovered Platforms into the workspace registry.
    Sync {
        /// Check whether generated files are current without writing them.
        #[arg(long)]
        check: bool,
    },
    /// Launch an application through one Platform's declared runner.
    Launch {
        /// Registered Platform name.
        platform: String,
        /// Application executable produced by Cargo.
        application: PathBuf,
        /// Arguments forwarded to the application launcher.
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        arguments: Vec<OsString>,
    },
}

fn main() -> ExitCode {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    match Cli::parse().command {
        Command::Sync { check } => match sync_with_report(&root, check) {
            Ok(report) => {
                let action = match report.status() {
                    SyncStatus::Updated => "Synced",
                    SyncStatus::Current => "Checked",
                };
                println!(
                    "{SUCCESS}✔{SUCCESS:#} {action} {EMPHASIS}Platform{EMPHASIS:#} registry {DIM}({} Platforms){DIM:#}.",
                    report.platforms().len()
                );
                ExitCode::SUCCESS
            }
            Err(error) => {
                eprintln!("{ERROR}error:{ERROR:#} {error}");
                ExitCode::FAILURE
            }
        },
        Command::Launch {
            platform,
            application,
            arguments,
        } => match launch(&root, &platform, &application, &arguments) {
            Ok(status) => ExitCode::from(
                status
                    .code()
                    .and_then(|code| u8::try_from(code).ok())
                    .unwrap_or(1),
            ),
            Err(error) => {
                eprintln!("{ERROR}error:{ERROR:#} {error}");
                ExitCode::FAILURE
            }
        },
    }
}
