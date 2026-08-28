//! `cargo board` entry point.

use std::{
    io,
    path::PathBuf,
    process::{self, ExitCode},
};

use barracuda_board_tool::{execute, Cli, Command};
use clap::Parser;
use dialoguer::console::Term;

fn main() -> ExitCode {
    let workspace_root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let cli = Cli::parse();
    if matches!(&cli.command, Command::Select { name: None }) {
        if let Err(error) = install_interrupt_handler() {
            eprintln!("error: failed to install Ctrl-C handler: {error}");
            return ExitCode::FAILURE;
        }
    }
    let mut output = io::stdout().lock();
    match execute(cli, &workspace_root, &mut output) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) if is_interrupted_prompt(&error) => {
            let _ = Term::stderr().show_cursor();
            ExitCode::from(130)
        }
        Err(error) => {
            eprintln!("error: {error}");
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

fn is_interrupted_prompt(error: &barracuda_board_tool::CommandError) -> bool {
    matches!(
        error,
        barracuda_board_tool::CommandError::Prompt(dialoguer::Error::IO(source))
            if source.kind() == io::ErrorKind::Interrupted
    )
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use barracuda_board_tool::CommandError;

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
