//! `cargo system-image` entry point.

use std::path::Path;
use std::process::ExitCode;

use barracuda_system_image::build_selected;
use clap::Parser;

/// Builds the top-level image tree for the selected Board's System region.
#[derive(Debug, Parser)]
#[command(name = "cargo system-image", bin_name = "cargo system-image", version)]
struct Cli {}

fn main() -> ExitCode {
    Cli::parse();
    build()
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
    fn command_accepts_no_build_parameters() {
        assert!(Cli::try_parse_from(["cargo system-image"]).is_ok());
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
            let error = Cli::try_parse_from(["cargo system-image", argument])
                .expect_err("build parameters are unsupported");
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
