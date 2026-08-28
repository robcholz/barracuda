//! `cargo plugin` workspace maintenance commands.

use std::{
    env,
    path::PathBuf,
    process::{self, ExitCode},
};

use anstream::{eprintln, println};
use anstyle::{AnsiColor, Color, Style};
use barracuda_plugin_tool::{configure, sync_with_report, SyncStatus};
use clap::{Parser, Subcommand};
use dialoguer::console::Term;

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
    let command = Cli::parse().command;
    if matches!(&command, Command::Select) {
        if let Err(error) = install_interrupt_handler() {
            eprintln!("error: failed to install Ctrl-C handler: {error}");
            return ExitCode::FAILURE;
        }
    }
    let result = match command {
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
        Err(error) if is_interrupted_prompt(&error) => {
            let _ = Term::stderr().show_cursor();
            ExitCode::from(130)
        }
        Err(error) => {
            let red = Style::new()
                .fg_color(Some(Color::Ansi(AnsiColor::Red)))
                .bold();
            eprintln!("{red}error:{red:#} {error}");
            ExitCode::FAILURE
        }
    }
}

fn install_interrupt_handler() -> Result<(), ctrlc::Error> {
    ctrlc::set_handler(|| {
        handle_interrupt(
            || {
                let _ = Term::stderr().show_cursor();
            },
            process::exit,
        );
    })
}

fn handle_interrupt<R, E, T>(restore_cursor: R, exit: E) -> T
where
    R: FnOnce(),
    E: FnOnce(i32) -> T,
{
    restore_cursor();
    exit(130)
}

fn is_interrupted_prompt(error: &barracuda_plugin_tool::CommandError) -> bool {
    matches!(
        error,
        barracuda_plugin_tool::CommandError::Prompt(dialoguer::Error::IO(source))
            if source.kind() == std::io::ErrorKind::Interrupted
    )
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

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use barracuda_plugin_tool::CommandError;

    #[test]
    fn interrupt_restores_cursor_before_exiting() {
        let events = RefCell::new(Vec::new());

        super::handle_interrupt(
            || events.borrow_mut().push("cursor"),
            |status| {
                assert_eq!(status, 130);
                events.borrow_mut().push("exit");
            },
        );

        assert_eq!(*events.borrow(), ["cursor", "exit"]);
    }

    #[test]
    fn interrupted_prompt_uses_interrupt_exit_path() {
        let error = CommandError::Prompt(dialoguer::Error::IO(std::io::Error::new(
            std::io::ErrorKind::Interrupted,
            "read interrupted",
        )));

        assert!(super::is_interrupted_prompt(&error));
    }
}
