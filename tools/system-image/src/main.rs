//! `cargo system-image` entry point.

use std::path::Path;
use std::process::ExitCode;

use barracuda_system_image::{build_selected, flash_selected};
use clap::{Parser, Subcommand};

/// Manages the selected Board's System image.
#[derive(Debug, Parser)]
#[command(
    name = "cargo system-image",
    bin_name = "cargo system-image",
    version,
    styles = barracuda_cli_style::CLI_STYLES
)]
struct Cli {
    /// System-image operation to perform.
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Build the selected Board's System partition image.
    Build,
    /// Flash the built image into the selected Board's System partition.
    Flash,
}

fn main() -> ExitCode {
    match Cli::parse().command {
        Command::Build => build(),
        Command::Flash => flash(),
    }
}

fn flash() -> ExitCode {
    let Some(workspace) = workspace_root(Path::new(env!("CARGO_MANIFEST_DIR"))) else {
        eprintln!("error: system-image crate is not located below the workspace root");
        return ExitCode::FAILURE;
    };
    match flash_selected(workspace) {
        Ok(image) => {
            println!(
                "Flashed {}-byte system image for Board `{}` at {:#x} from `{}` to `{}`.",
                image.size(),
                image.board(),
                image.offset(),
                image.image().display(),
                image.destination()
            );
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("error: {error}");
            ExitCode::FAILURE
        }
    }
}

fn build() -> ExitCode {
    let Some(workspace) = workspace_root(Path::new(env!("CARGO_MANIFEST_DIR"))) else {
        eprintln!("error: system-image crate is not located below the workspace root");
        return ExitCode::FAILURE;
    };
    match build_selected(workspace) {
        Ok(image) => {
            println!(
                "Built {}-byte system image for Board `{}` region at {:#x} from `{}` at `{}`.",
                image.size(),
                image.board(),
                image.offset(),
                image.source().display(),
                image.output().display()
            );
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("error: {error}");
            ExitCode::FAILURE
        }
    }
}

fn workspace_root(manifest_dir: &Path) -> Option<&Path> {
    manifest_dir.ancestors().nth(2)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use clap::{error::ErrorKind, Parser};

    use super::Cli;

    #[test]
    fn command_requires_an_operation() {
        let error = Cli::try_parse_from(["cargo system-image"])
            .expect_err("an explicit operation is required");

        assert_eq!(
            error.kind(),
            ErrorKind::DisplayHelpOnMissingArgumentOrSubcommand
        );
    }

    #[test]
    fn command_accepts_build_without_parameters() {
        assert!(Cli::try_parse_from(["cargo system-image", "build"]).is_ok());
    }

    #[test]
    fn command_accepts_flash_without_parameters() {
        assert!(Cli::try_parse_from(["cargo system-image", "flash"]).is_ok());
    }

    #[test]
    fn command_exposes_clap_help() {
        let error = Cli::try_parse_from(["cargo system-image", "--help"])
            .expect_err("help exits before building");

        assert_eq!(error.kind(), ErrorKind::DisplayHelp);
        assert!(error.to_string().contains("Usage: cargo system-image"));
    }

    #[test]
    fn command_rejects_build_parameters() {
        for argument in ["--size", "--output"] {
            let error = Cli::try_parse_from(["cargo system-image", "build", argument])
                .expect_err("build parameters are unsupported");
            assert_eq!(error.kind(), ErrorKind::UnknownArgument);
        }
    }

    #[test]
    fn command_rejects_flash_parameters() {
        for argument in ["--board", "--platform", "--image"] {
            let error = Cli::try_parse_from(["cargo system-image", "flash", argument])
                .expect_err("flash parameters are unsupported");
            assert_eq!(error.kind(), ErrorKind::UnknownArgument);
        }
    }

    #[test]
    fn resolves_the_workspace_above_the_tool_crate() {
        let manifest_dir = std::path::Path::new("workspace/tools/system-image");

        assert_eq!(
            super::workspace_root(manifest_dir),
            Some(std::path::Path::new("workspace"))
        );
    }
}
