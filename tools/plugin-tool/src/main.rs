//! `cargo plugin` workspace maintenance commands.

use std::{
    env,
    path::PathBuf,
    process::{self, ExitCode},
};

use anstream::{eprintln, println};
use barracuda_cli_style::{DIM, EMPHASIS, ERROR, SUCCESS, WARNING};
use barracuda_plugin_tool::{configure, info, sync_with_report, PluginInfo, SyncStatus};
use clap::{Parser, Subcommand};
use dialoguer::console::Term;

#[derive(Debug, Parser)]
#[command(
    name = "cargo plugin",
    bin_name = "cargo plugin",
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
    /// Run an author-declared task for enabled Plugins.
    Run {
        /// Task name from plugin.toml.
        task: String,
        /// Run only this enabled Plugin's task.
        #[arg(long)]
        plugin: Option<String>,
    },
    /// Synchronize discovered Plugins into the System registry.
    Sync {
        /// Check whether generated files are current without writing them.
        #[arg(long)]
        check: bool,
    },
    /// Interactively choose which Plugins are enabled.
    Select,
    /// Show manifest and dependency information for one Plugin.
    Info {
        /// Plugin identity or directory name.
        plugin: String,
    },
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
        Command::Run { task, plugin } => {
            barracuda_plugin_tool::run(&root, &task, plugin.as_deref())
        }
        Command::Sync { check } => sync_with_report(&root, check).map(|report| {
            print_report(&report);
        }),
        Command::Select => configure(&root).and_then(|()| {
            let report = sync_with_report(&root, false)?;
            print_report(&report);
            Ok(())
        }),
        Command::Info { plugin } => info(&root, &plugin).map(|plugin| print_info(&plugin)),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) if is_interrupted_prompt(&error) => {
            let _ = Term::stderr().show_cursor();
            ExitCode::from(130)
        }
        Err(error) => {
            eprintln!("{ERROR}error:{ERROR:#} {error}");
            ExitCode::FAILURE
        }
    }
}

fn print_info(plugin: &PluginInfo) {
    let (status_style, status) = if plugin.enabled() {
        (SUCCESS, "enabled")
    } else {
        (WARNING, "disabled")
    };
    println!(
        "{EMPHASIS}{}{EMPHASIS:#}  {}",
        plugin.id(),
        plugin.description()
    );
    println!("  {DIM}directory{DIM:#}     {}", plugin.directory());
    println!("  {DIM}status{DIM:#}        {status_style}{status}{status_style:#}");
    print_list("depends on", plugin.dependencies());
    print_list("required by", plugin.dependents());
}

fn print_list(label: &str, values: &[String]) {
    let value = if values.is_empty() {
        String::from("—")
    } else {
        values.join(", ")
    };
    println!("  {DIM}{label:<13}{DIM:#} {value}");
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
    let action = match report.status() {
        SyncStatus::Updated => "Synced",
        SyncStatus::Current => "Checked",
    };
    println!("{SUCCESS}✔{SUCCESS:#} {action} {EMPHASIS}Plugin{EMPHASIS:#} registry");
    for plugin in report.enabled() {
        println!("  {SUCCESS}●{SUCCESS:#} {plugin}");
    }
    for plugin in report.disabled() {
        println!("  {WARNING}○{WARNING:#} {plugin} {DIM}(disabled){DIM:#}");
    }
    println!(
        "{DIM}  {} enabled, {} disabled{DIM:#}",
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
