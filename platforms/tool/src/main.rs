//! `cargo platform` workspace maintenance commands.

use std::{env, path::PathBuf, process::ExitCode};

use barracuda_platform_tool::{sync_with_report, SyncStatus};
use clap::{Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(name = "cargo platform", bin_name = "cargo platform", version, about)]
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
                    "{action} Platform registry ({} Platforms).",
                    report.platforms().len()
                );
                ExitCode::SUCCESS
            }
            Err(error) => {
                eprintln!("error: {error}");
                ExitCode::FAILURE
            }
        },
    }
}
