//! `cargo plugin` workspace maintenance commands.

use std::{env, path::PathBuf, process::ExitCode};

use anstream::{eprintln, println};
use anstyle::{AnsiColor, Color, Style};
use barracuda_plugin_tool::{configure, sync_with_report, SyncStatus};
use clap::{Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(name = "cargo plugin", bin_name = "cargo plugin", version, about)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Synchronize discovered Plugins into the System registry.
    Sync {
        /// Check whether generated files are current without writing them.
        #[arg(long)]
        check: bool,
    },
    /// Interactively choose which Plugins are enabled.
    Select,
}

fn main() -> ExitCode {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let result = match Cli::parse().command {
        Command::Sync { check } => sync_with_report(&root, check).map(|report| {
            print_report(&report);
        }),
        Command::Select => configure(&root).and_then(|()| {
            let report = sync_with_report(&root, false)?;
            print_report(&report);
            Ok(())
        }),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            let red = Style::new()
                .fg_color(Some(Color::Ansi(AnsiColor::Red)))
                .bold();
            eprintln!("{red}error:{red:#} {error}");
            ExitCode::FAILURE
        }
    }
}

fn print_report(report: &barracuda_plugin_tool::SyncReport) {
    let green = Style::new()
        .fg_color(Some(Color::Ansi(AnsiColor::Green)))
        .bold();
    let yellow = Style::new().fg_color(Some(Color::Ansi(AnsiColor::Yellow)));
    let dim = Style::new().dimmed();
    let action = match report.status() {
        SyncStatus::Updated => "Synced",
        SyncStatus::Current => "Checked",
    };
    println!("{green}✔{green:#} {action} Plugin registry");
    for plugin in report.enabled() {
        println!("  {green}●{green:#} {plugin}");
    }
    for plugin in report.disabled() {
        println!("  {yellow}○{yellow:#} {plugin} {dim}(disabled){dim:#}");
    }
    println!(
        "{dim}  {} enabled, {} disabled{dim:#}",
        report.enabled().len(),
        report.disabled().len()
    );
}
