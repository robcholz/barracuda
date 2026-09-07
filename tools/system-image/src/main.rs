//! `cargo image` entry point.

use std::path::Path;
use std::process::ExitCode;

use anstream::{eprintln, println};
use barracuda_cli_style::{DIM, EMPHASIS, ERROR, SUCCESS};
use barracuda_system_image::{
    build_selected, deploy_selected, flash_selected, ResourcesFilesystem,
};
use clap::{Parser, Subcommand};

/// Manages the selected Board's bundled Plugin resource image.
#[derive(Debug, Parser)]
#[command(
    name = "cargo image",
    bin_name = "cargo image",
    version,
    styles = barracuda_cli_style::CLI_STYLES
)]
struct Cli {
    /// Image operation to perform.
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Build the selected Board's configured resource image.
    Build,
    /// Flash the built image into the selected Board's `resources` partition.
    Flash,
    /// Build and flash the selected Board's configured resource image.
    Deploy,
}

fn main() -> ExitCode {
    match Cli::parse().command {
        Command::Build => build(),
        Command::Flash => flash(),
        Command::Deploy => deploy(),
    }
}

fn deploy() -> ExitCode {
    let Some(workspace) = workspace_root(Path::new(env!("CARGO_MANIFEST_DIR"))) else {
        eprintln!(
            "{ERROR}error:{ERROR:#} image tool crate is not located below the workspace root"
        );
        return ExitCode::FAILURE;
    };
    match deploy_selected(workspace) {
        Ok(image) => {
            println!(
                "{SUCCESS}✔{SUCCESS:#} Deployed {DIM}{}-byte {} resource image{DIM:#} for {EMPHASIS}Board `{}`{EMPHASIS:#} at {DIM}{:#x}{DIM:#} from `{}` to `{}`.",
                image.size(),
                filesystem_name(image.filesystem()),
                image.board(),
                image.offset(),
                image.image().display(),
                image.destination()
            );
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("{ERROR}error:{ERROR:#} {error}");
            ExitCode::FAILURE
        }
    }
}

fn flash() -> ExitCode {
    let Some(workspace) = workspace_root(Path::new(env!("CARGO_MANIFEST_DIR"))) else {
        eprintln!(
            "{ERROR}error:{ERROR:#} image tool crate is not located below the workspace root"
        );
        return ExitCode::FAILURE;
    };
    match flash_selected(workspace) {
        Ok(image) => {
            println!(
                "{SUCCESS}✔{SUCCESS:#} Flashed {DIM}{}-byte {} resource image{DIM:#} for {EMPHASIS}Board `{}`{EMPHASIS:#} at {DIM}{:#x}{DIM:#} from `{}` to `{}`.",
                image.size(),
                filesystem_name(image.filesystem()),
                image.board(),
                image.offset(),
                image.image().display(),
                image.destination()
            );
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("{ERROR}error:{ERROR:#} {error}");
            ExitCode::FAILURE
        }
    }
}

fn build() -> ExitCode {
    let Some(workspace) = workspace_root(Path::new(env!("CARGO_MANIFEST_DIR"))) else {
        eprintln!(
            "{ERROR}error:{ERROR:#} image tool crate is not located below the workspace root"
        );
        return ExitCode::FAILURE;
    };
    match build_selected(workspace) {
        Ok(image) => {
            println!(
                "{SUCCESS}✔{SUCCESS:#} Built {DIM}{}-byte {} resource image{DIM:#} for {EMPHASIS}Board `{}`{EMPHASIS:#} `resources` region at {DIM}{:#x}{DIM:#} from enabled Plugin resources at `{}`.",
                image.size(),
                filesystem_name(image.filesystem()),
                image.board(),
                image.offset(),
                image.output().display()
            );
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("{ERROR}error:{ERROR:#} {error}");
            ExitCode::FAILURE
        }
    }
}

const fn filesystem_name(filesystem: ResourcesFilesystem) -> &'static str {
    match filesystem {
        ResourcesFilesystem::FatFs => "FATFS",
        ResourcesFilesystem::LittleFs => "LittleFS",
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
        let error =
            Cli::try_parse_from(["cargo image"]).expect_err("an explicit operation is required");

        assert_eq!(
            error.kind(),
            ErrorKind::DisplayHelpOnMissingArgumentOrSubcommand
        );
    }

    #[test]
    fn command_accepts_build_without_parameters() {
        assert!(Cli::try_parse_from(["cargo image", "build"]).is_ok());
    }

    #[test]
    fn command_accepts_flash_without_parameters() {
        assert!(Cli::try_parse_from(["cargo image", "flash"]).is_ok());
    }

    #[test]
    fn command_accepts_deploy_without_parameters() {
        assert!(Cli::try_parse_from(["cargo image", "deploy"]).is_ok());
    }

    #[test]
    fn command_exposes_clap_help() {
        let error =
            Cli::try_parse_from(["cargo image", "--help"]).expect_err("help exits before building");

        assert_eq!(error.kind(), ErrorKind::DisplayHelp);
        assert!(error.to_string().contains("Usage: cargo image"));
    }

    #[test]
    fn command_rejects_build_parameters() {
        for argument in ["--size", "--output"] {
            let error = Cli::try_parse_from(["cargo image", "build", argument])
                .expect_err("build parameters are unsupported");
            assert_eq!(error.kind(), ErrorKind::UnknownArgument);
        }
    }

    #[test]
    fn command_rejects_flash_parameters() {
        for argument in ["--board", "--platform", "--image"] {
            let error = Cli::try_parse_from(["cargo image", "flash", argument])
                .expect_err("flash parameters are unsupported");
            assert_eq!(error.kind(), ErrorKind::UnknownArgument);
        }
    }

    #[test]
    fn command_rejects_deploy_parameters() {
        for argument in ["--board", "--platform", "--image"] {
            let error = Cli::try_parse_from(["cargo image", "deploy", argument])
                .expect_err("deploy parameters are unsupported");
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
