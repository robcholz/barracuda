//! Default `cargo run` entry point for the selected Barracuda target.

use std::env;
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

fn main() -> ExitCode {
    match run() {
        Ok(status) => ExitCode::from(status),
        Err(error) => {
            eprintln!("error: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<u8, String> {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let generated = root.join("target/barracuda-workspace");
    let selection = barracuda_build::prepare_selected_workspace(&root, &generated)?;
    eprintln!(
        "Building Board `{}` with Platform `{}`",
        selection.board(),
        selection.platform()
    );

    let mut arguments = env::args_os().skip(1).collect::<Vec<_>>();
    let operation = match arguments.first().and_then(|argument| argument.to_str()) {
        Some("build") => {
            arguments.remove(0);
            "build"
        }
        Some("run") => {
            arguments.remove(0);
            "run"
        }
        _ => "run",
    };
    let cargo = env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let target_directory = root.join("target");
    let mut build = cargo_command(&cargo, &generated, &target_directory, &selection, "build");
    build.args(["--package", "barracuda-cli", "--bin", "barracuda"]);
    if operation == "build" {
        build.args(&arguments);
    }
    successful(&mut build, "selected application build")?;

    for binary in selection.application().support_binaries() {
        let mut support = cargo_command(&cargo, &generated, &target_directory, &selection, "build");
        support
            .arg("--package")
            .arg(selection.platform_package())
            .arg("--bin")
            .arg(binary);
        if operation == "build" {
            support.args(&arguments);
        }
        successful(&mut support, "Platform support binary build")?;
    }
    if operation == "build" {
        return Ok(0);
    }

    let binary_directory = selection.target().map_or_else(
        || target_directory.join("debug"),
        |target| target_directory.join(target).join("debug"),
    );
    let application = binary_directory.join("barracuda");
    let mut command = if let Some(launcher) = selection.application().launcher() {
        let program = resolve_program(launcher.program(), selection.platform_directory());
        let mut command = Command::new(program);
        for argument in launcher.arguments() {
            command.arg(expand_launcher_argument(
                argument,
                &root,
                &target_directory,
                &application,
                &binary_directory,
                selection.application().support_binaries(),
            )?);
        }
        command
    } else {
        Command::new(&application)
    };
    command.args(arguments).current_dir(&root);
    let status = command
        .status()
        .map_err(|error| format!("failed to launch `{}`: {error}", application.display()))?;
    Ok(status
        .code()
        .and_then(|code| u8::try_from(code).ok())
        .unwrap_or(1))
}

fn cargo_command(
    cargo: &OsStr,
    generated: &Path,
    target_directory: &Path,
    selection: &barracuda_build::BuildSelection,
    operation: &str,
) -> Command {
    let mut command = Command::new(cargo);
    command
        .arg(operation)
        .arg("--manifest-path")
        .arg(generated.join("Cargo.toml"))
        .arg("--target-dir")
        .arg(target_directory)
        .env("BARRACUDA_GENERATED_BUILD", "1")
        .env("BARRACUDA_PLATFORM", selection.platform())
        .current_dir(generated);
    if let Some(target) = selection.target() {
        command.arg("--target").arg(target);
    }
    command
}

fn successful(command: &mut Command, kind: &str) -> Result<(), String> {
    let status = command
        .status()
        .map_err(|error| format!("failed to start {kind}: {error}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("{kind} exited with {status}"))
    }
}

fn resolve_program(program: &Path, platform: &Path) -> PathBuf {
    if program.components().count() == 1 || program.is_absolute() {
        program.to_path_buf()
    } else {
        platform.join(program)
    }
}

fn expand_launcher_argument(
    argument: &str,
    workspace: &Path,
    target_directory: &Path,
    application: &Path,
    binary_directory: &Path,
    support_binaries: &[String],
) -> Result<OsString, String> {
    if argument == "{application}" {
        return Ok(application.as_os_str().to_owned());
    }
    if argument == "{workspace}" {
        return Ok(workspace.as_os_str().to_owned());
    }
    if argument == "{target-dir}" {
        return Ok(target_directory.as_os_str().to_owned());
    }
    if let Some(binary) = argument
        .strip_prefix("{support:")
        .and_then(|value| value.strip_suffix('}'))
    {
        if support_binaries.iter().any(|candidate| candidate == binary) {
            return Ok(binary_directory.join(binary).into_os_string());
        }
        return Err(format!(
            "launcher references undeclared support binary `{binary}`"
        ));
    }
    Ok(OsString::from(argument))
}
