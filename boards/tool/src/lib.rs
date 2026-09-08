//! Board selection command support.

use std::{
    env,
    ffi::OsString,
    fs,
    io::{self, Write},
    path::{Path, PathBuf},
    process::Command as ProcessCommand,
};

use barracuda_board_config::{
    parse, read_selected_board, validate_board_name, write_selected_board, BoardDefinition,
    ConfigError, SelectionError,
};
use barracuda_driver_config::{
    load_catalog, render_board_hal_for_platform, resolve_board, validate_platform_hal,
    CatalogError, GenerateError, ResolveError as DriverResolveError,
};
use barracuda_platform_config::{resolve_board_platform, ResolveError as PlatformResolveError};
use clap::{Parser, Subcommand};
use dialoguer::{theme::ColorfulTheme, FuzzySelect};

const DRIVER_WORKSPACE_BEGIN: &str = "# BEGIN GENERATED DRIVER WORKSPACE DEPENDENCIES";
const DRIVER_WORKSPACE_END: &str = "# END GENERATED DRIVER WORKSPACE DEPENDENCIES";

/// Whether Board registry synchronization changed generated files.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SyncStatus {
    /// The generated registry was rewritten.
    Updated,
    /// The generated registry was already current, or was only checked.
    Current,
}

/// Summary of one Board registry synchronization.
#[derive(Debug, Eq, PartialEq)]
pub struct SyncReport {
    status: SyncStatus,
    boards: usize,
    drivers: usize,
}

impl SyncReport {
    /// Returns whether generated source changed.
    #[must_use]
    pub const fn status(&self) -> SyncStatus {
        self.status
    }

    /// Returns the number of discovered Board bundles.
    #[must_use]
    pub const fn boards(&self) -> usize {
        self.boards
    }

    /// Returns the number of discovered Driver manifests.
    #[must_use]
    pub const fn drivers(&self) -> usize {
        self.drivers
    }
}

/// Command-line interface for `cargo board`.
#[derive(Debug, Parser)]
#[command(
    name = "cargo board",
    bin_name = "cargo board",
    version,
    about,
    styles = barracuda_cli_style::CLI_STYLES
)]
pub struct Cli {
    /// Board operation to perform.
    #[command(subcommand)]
    pub command: Command,
}

/// Operations supported by `cargo board`.
#[derive(Debug, Subcommand)]
pub enum Command {
    /// Validate Boards and synchronize Drivers into the workspace registry.
    Sync {
        /// Check whether generated files are current without writing them.
        #[arg(long)]
        check: bool,
    },
    /// Select a Board, interactively when no name is provided.
    Select {
        /// Board bundle name under `boards/configs`.
        name: Option<String>,
    },
    /// Print a Board's Rust target triple.
    Target {
        /// Board bundle name; defaults to the selected Board.
        name: Option<String>,
    },
    /// Internal Cargo target runner installed by Board selection.
    #[command(name = "__run", hide = true)]
    Run {
        /// Selected Platform name.
        platform: String,
        /// Cargo-produced executable.
        application: PathBuf,
        /// Arguments forwarded to the executable.
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        arguments: Vec<OsString>,
    },
}

/// Failure while selecting a Board bundle.
#[derive(Debug, thiserror::Error)]
pub enum CommandError {
    /// Command-line arguments are invalid.
    #[error(transparent)]
    Arguments(#[from] clap::Error),
    /// The requested Board bundle does not exist.
    #[error("Board `{name}` does not exist under `boards/configs`")]
    BoardMissing {
        /// Requested Board name.
        name: String,
    },
    /// The bundle directory does not agree with its YAML identity.
    #[error("Board directory `{directory}` declares Board `{declared}`")]
    NameMismatch {
        /// Bundle directory name.
        directory: String,
        /// Name declared by `board.yml`.
        declared: String,
    },
    /// The Board bundle does not contain its declared native layout.
    #[error("Board `{name}` native layout `{path}` does not exist")]
    NativeLayoutMissing {
        /// Selected Board name.
        name: String,
        /// Missing native-layout path.
        path: PathBuf,
    },
    /// A Board configuration is invalid.
    #[error(transparent)]
    Config(#[from] ConfigError),
    /// The Driver catalog is invalid or unavailable.
    #[error(transparent)]
    DriverCatalog(#[from] CatalogError),
    /// A Board does not satisfy its selected Driver schemas.
    #[error(transparent)]
    DriverComposition(#[from] DriverResolveError),
    /// A resolved Board cannot be rendered into static Rust bindings.
    #[error(transparent)]
    DriverGeneration(#[from] GenerateError),
    /// The Board's Platform could not be resolved.
    #[error(transparent)]
    Platform(#[from] PlatformResolveError),
    /// Persistent selection state is invalid or unavailable.
    #[error(transparent)]
    Selection(#[from] SelectionError),
    /// A Board bundle could not be read.
    #[error("failed to read Board bundle `{path}`: {source}")]
    Read {
        /// File that could not be read.
        path: PathBuf,
        /// Underlying filesystem failure.
        #[source]
        source: io::Error,
    },
    /// The Board catalog directory could not be read.
    #[error("failed to read Board catalog `{path}`: {source}")]
    Catalog {
        /// Directory or entry that could not be read.
        path: PathBuf,
        /// Underlying filesystem failure.
        #[source]
        source: io::Error,
    },
    /// The Board catalog contains no selectable bundles.
    #[error("no Board bundles found under `boards/configs`")]
    EmptyCatalog,
    /// A command needs a Board but none has been selected.
    #[error("no Board selected; run `cargo board select` first")]
    NoSelection,
    /// The terminal prompt failed.
    #[error("interactive Board selection failed: {0}")]
    Prompt(#[source] dialoguer::Error),
    /// An interactive selector returned an invalid catalog position.
    #[error("interactive Board selection returned invalid index {index}")]
    InvalidSelectionIndex {
        /// Invalid index returned by the selector.
        index: usize,
    },
    /// Command output could not be written.
    #[error("failed to write command output: {0}")]
    Output(#[source] io::Error),
    /// A generated registry block is malformed.
    #[error("{0}")]
    Generated(String),
    /// Generated Board dependencies do not match the catalog.
    #[error("Board registry is stale; run `cargo board sync`")]
    StaleRegistry,
    /// The local target runner executable could not be installed.
    #[error("failed to install target runner `{path}`: {source}")]
    RunnerInstall {
        /// Runner path that could not be written.
        path: PathBuf,
        /// Underlying filesystem error.
        #[source]
        source: io::Error,
    },
    /// A directly executed Cargo tool could not be started.
    #[error("failed to start selected executable `{path}`: {source}")]
    RunnerStart {
        /// Executable Cargo asked the runner to start.
        path: PathBuf,
        /// Underlying process error.
        #[source]
        source: io::Error,
    },
    /// A selected executable exited unsuccessfully.
    #[error("selected executable exited with {0}")]
    RunnerFailed(std::process::ExitStatus),
    /// Platform launch orchestration failed.
    #[error(transparent)]
    PlatformLaunch(#[from] barracuda_platform_tool::CommandError),
}

/// Runs the workspace Board command against `workspace_root`.
///
/// # Errors
///
/// Returns [`CommandError`] when the command syntax is invalid, the requested
/// Board bundle is incomplete, or the selection cannot be persisted.
pub fn run<I, S, W>(args: I, workspace_root: &Path, output: &mut W) -> Result<(), CommandError>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
    W: Write,
{
    let cli = Cli::try_parse_from(
        std::iter::once(String::from("cargo board")).chain(
            args.into_iter()
                .map(|argument| argument.as_ref().to_owned()),
        ),
    )?;
    execute_with_selector(cli, workspace_root, output, prompt_for_board, false)
}

/// Executes a parsed Board command.
///
/// # Errors
/// Returns [`CommandError`] when discovery, validation, or persistence fails.
pub fn execute<W: Write>(
    cli: Cli,
    workspace_root: &Path,
    output: &mut W,
) -> Result<(), CommandError> {
    execute_with_selector(cli, workspace_root, output, prompt_for_board, false)
}

/// Executes a parsed Board command with terminal styling enabled.
pub fn execute_colored<W: Write>(
    cli: Cli,
    workspace_root: &Path,
    output: &mut W,
) -> Result<(), CommandError> {
    execute_with_selector(cli, workspace_root, output, prompt_for_board, true)
}

fn execute_with_selector<W, F>(
    cli: Cli,
    workspace_root: &Path,
    output: &mut W,
    selector: F,
    color: bool,
) -> Result<(), CommandError>
where
    W: Write,
    F: FnOnce(&[String], Option<usize>) -> Result<Option<usize>, CommandError>,
{
    match cli.command {
        Command::Sync { check } => {
            let report = sync_with_report(workspace_root, check)?;
            let action = match report.status() {
                SyncStatus::Updated => "Synced",
                SyncStatus::Current => "Checked",
            };
            writeln!(
                output,
                "{}{action} {} registry ({} Boards, {} Drivers).",
                prefix(color, barracuda_cli_style::SUCCESS, "✔"),
                styled(color, barracuda_cli_style::EMPHASIS, "Board"),
                report.boards(),
                report.drivers()
            )
            .map_err(CommandError::Output)
        }
        Command::Select { name: None } => {
            let boards = discover_boards(workspace_root)?;
            let current = read_selected_board(workspace_root)?;
            let default = current
                .as_ref()
                .and_then(|current| boards.iter().position(|board| board == current));
            let Some(index) = selector(&boards, default)? else {
                writeln!(
                    output,
                    "{}Board selection cancelled.",
                    prefix(color, barracuda_cli_style::WARNING, "⚠")
                )
                .map_err(CommandError::Output)?;
                return Ok(());
            };
            let name = boards
                .get(index)
                .ok_or(CommandError::InvalidSelectionIndex { index })?;
            select_board(workspace_root, name, output, color)
        }
        Command::Select { name: Some(name) } => select_board(workspace_root, &name, output, color),
        Command::Target { name: None } => {
            let name = read_selected_board(workspace_root)?.ok_or(CommandError::NoSelection)?;
            print_target(workspace_root, &name, output)
        }
        Command::Target { name: Some(name) } => print_target(workspace_root, &name, output),
        Command::Run {
            platform,
            application,
            arguments,
        } => run_selected_application(workspace_root, &platform, &application, &arguments),
    }
}

fn run_selected_application(
    workspace_root: &Path,
    platform: &str,
    application: &Path,
    arguments: &[OsString],
) -> Result<(), CommandError> {
    let status = if requires_platform_launcher(application) {
        barracuda_platform_tool::launch(workspace_root, platform, application, arguments)?
    } else {
        ProcessCommand::new(application)
            .args(arguments)
            .current_dir(workspace_root)
            .status()
            .map_err(|source| CommandError::RunnerStart {
                path: application.to_owned(),
                source,
            })?
    };
    if status.success() {
        Ok(())
    } else {
        Err(CommandError::RunnerFailed(status))
    }
}

fn requires_platform_launcher(application: &Path) -> bool {
    application.file_stem().and_then(|name| name.to_str()) == Some("barracuda-system")
}

/// Synchronizes or validates the tracked Board HAL registry.
///
/// # Errors
///
/// Returns an error when Board discovery, generated markers, or file access fails.
pub fn sync(workspace_root: &Path, check: bool) -> Result<(), CommandError> {
    sync_with_report(workspace_root, check).map(|_report| ())
}

/// Validates discovered Boards and synchronizes Drivers into the workspace registry.
///
/// # Errors
///
/// Returns an error when Board discovery, generated markers, or file access fails.
pub fn sync_with_report(workspace_root: &Path, check: bool) -> Result<SyncReport, CommandError> {
    let boards = discover_boards(workspace_root)?;
    for name in &boards {
        read_board(workspace_root, name)?;
    }
    let catalog = load_catalog(workspace_root)?;
    let driver_count = catalog.drivers().count();

    let manifest_path = workspace_root.join("Cargo.toml");
    let old_manifest = fs::read_to_string(&manifest_path).map_err(|source| CommandError::Read {
        path: manifest_path.clone(),
        source,
    })?;
    let driver_dependencies = catalog
        .drivers()
        .map(|driver| {
            format!(
                "{} = {{ path = {:?} }}",
                driver.implementation().package(),
                PathBuf::from("drivers").join(driver.id()).to_string_lossy()
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    let manifest = replace_block(
        &old_manifest,
        DRIVER_WORKSPACE_BEGIN,
        DRIVER_WORKSPACE_END,
        &driver_dependencies,
    )?;
    let stale = manifest != old_manifest;
    if check && stale {
        return Err(CommandError::StaleRegistry);
    }
    if stale {
        fs::write(&manifest_path, manifest).map_err(|source| CommandError::Read {
            path: manifest_path,
            source,
        })?;
    }
    Ok(SyncReport {
        status: if stale {
            SyncStatus::Updated
        } else {
            SyncStatus::Current
        },
        boards: boards.len(),
        drivers: driver_count,
    })
}

fn replace_block(text: &str, begin: &str, end: &str, body: &str) -> Result<String, CommandError> {
    let begin_offset = text.find(begin).ok_or_else(|| {
        CommandError::Generated(format!("missing generated block marker `{begin}`"))
    })?;
    let line_start = text[..begin_offset]
        .rfind('\n')
        .map_or(0, |offset| offset + 1);
    let indentation = &text[line_start..begin_offset];
    if !indentation.bytes().all(|byte| matches!(byte, b' ' | b'\t')) {
        return Err(CommandError::Generated(format!(
            "invalid indentation before `{begin}`"
        )));
    }
    let end_offset = text[begin_offset..]
        .find(end)
        .map(|offset| begin_offset + offset)
        .ok_or_else(|| {
            CommandError::Generated(format!("missing generated block marker `{end}`"))
        })?;
    let line_end = text[end_offset..]
        .find('\n')
        .map_or(text.len(), |offset| end_offset + offset);
    Ok(format!(
        "{}{}{}\n{}\n{}{}{}",
        &text[..line_start],
        indentation,
        begin,
        body,
        indentation,
        end,
        &text[line_end..]
    ))
}

#[cfg(test)]
fn run_with_selector<I, S, W, F>(
    args: I,
    workspace_root: &Path,
    output: &mut W,
    selector: F,
) -> Result<(), CommandError>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
    W: Write,
    F: FnOnce(&[String], Option<usize>) -> Result<Option<usize>, CommandError>,
{
    let cli = Cli::try_parse_from(
        std::iter::once(String::from("cargo board")).chain(
            args.into_iter()
                .map(|argument| argument.as_ref().to_owned()),
        ),
    )?;
    execute_with_selector(cli, workspace_root, output, selector, false)
}

fn prompt_for_board(
    boards: &[String],
    default: Option<usize>,
) -> Result<Option<usize>, CommandError> {
    let theme = ColorfulTheme::default();
    FuzzySelect::with_theme(&theme)
        .with_prompt("Select a Board — type to search")
        .default(default.unwrap_or(0))
        .items(boards)
        .interact_opt()
        .map_err(CommandError::Prompt)
}

fn discover_boards(workspace_root: &Path) -> Result<Vec<String>, CommandError> {
    let catalog = workspace_root.join("boards/configs");
    let entries = fs::read_dir(&catalog).map_err(|source| CommandError::Catalog {
        path: catalog.clone(),
        source,
    })?;
    let mut boards = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|source| CommandError::Catalog {
            path: catalog.clone(),
            source,
        })?;
        let file_type = entry.file_type().map_err(|source| CommandError::Catalog {
            path: entry.path(),
            source,
        })?;
        if !file_type.is_dir() {
            continue;
        }
        let Ok(name) = entry.file_name().into_string() else {
            continue;
        };
        if validate_board_name(&name).is_ok() {
            boards.push(name);
        }
    }
    boards.sort_unstable();
    if boards.is_empty() {
        Err(CommandError::EmptyCatalog)
    } else {
        Ok(boards)
    }
}

/// Reads and validates a Board bundle, checking its declared name matches its
/// directory. Shared by `select` and `target`.
fn read_board(workspace_root: &Path, name: &str) -> Result<BoardDefinition, CommandError> {
    validate_board_name(name)?;
    let board_path = workspace_root
        .join("boards/configs")
        .join(name)
        .join("board.yml");
    let yaml = match fs::read_to_string(&board_path) {
        Ok(yaml) => yaml,
        Err(source) if source.kind() == io::ErrorKind::NotFound => {
            return Err(CommandError::BoardMissing {
                name: name.to_owned(),
            });
        }
        Err(source) => {
            return Err(CommandError::Read {
                path: board_path,
                source,
            });
        }
    };
    let board = parse(&yaml)?;
    if board.name() != name {
        return Err(CommandError::NameMismatch {
            directory: name.to_owned(),
            declared: board.name().to_owned(),
        });
    }
    if board.has_hardware_surface() {
        let catalog = load_catalog(workspace_root)?;
        let resolved = resolve_board(&board, &catalog)?;
        let platform = resolve_board_platform(
            workspace_root,
            board.hardware().chip(),
            board.toolchain().map(|toolchain| toolchain.target()),
        )?;
        validate_platform_hal(&board, &resolved, &platform)?;
        render_board_hal_for_platform(&board, &resolved, &platform)?;
    }
    Ok(board)
}

/// Prints the Board's declared cross-compilation target triple, so a build
/// driver reads the toolchain from the Board itself. A host Board declares no
/// toolchain and prints nothing (build for the host's native target).
fn print_target<W: Write>(
    workspace_root: &Path,
    name: &str,
    output: &mut W,
) -> Result<(), CommandError> {
    let board = read_board(workspace_root, name)?;
    match board.toolchain() {
        Some(toolchain) => writeln!(output, "{}", toolchain.target()).map_err(CommandError::Output),
        None => Ok(()),
    }
}

fn styled(color: bool, style: anstyle::Style, value: &str) -> String {
    if color {
        format!("{style}{value}{style:#}")
    } else {
        value.to_owned()
    }
}

fn prefix(color: bool, style: anstyle::Style, value: &str) -> String {
    if color {
        format!("{style}{value}{style:#} ")
    } else {
        String::new()
    }
}

fn select_board<W: Write>(
    workspace_root: &Path,
    name: &str,
    output: &mut W,
    color: bool,
) -> Result<(), CommandError> {
    let bundle = workspace_root.join("boards/configs").join(name);
    let board = read_board(workspace_root, name)?;
    let native_layout = bundle.join(board.native_layout().artifact());
    if !native_layout.is_file() {
        return Err(CommandError::NativeLayoutMissing {
            name: name.to_owned(),
            path: native_layout,
        });
    }

    write_selected_build(workspace_root, &board)?;
    write_selected_board(workspace_root, name)?;
    writeln!(
        output,
        "{}Selected Board `{}`.",
        prefix(color, barracuda_cli_style::SUCCESS, "✔"),
        styled(color, barracuda_cli_style::EMPHASIS, name)
    )
    .map_err(CommandError::Output)?;
    writeln!(
        output,
        "{}",
        styled(
            color,
            barracuda_cli_style::DIM,
            "Run `cargo run` to build and start it.",
        )
    )
    .map_err(CommandError::Output)
}

fn write_selected_build(
    workspace_root: &Path,
    board: &BoardDefinition,
) -> Result<(), CommandError> {
    let platform = resolve_board_platform(
        workspace_root,
        board.hardware().chip(),
        board.toolchain().map(|toolchain| toolchain.target()),
    )?;
    let platform_features = platform.cargo_features_for_chip(board.hardware().chip());
    let platform_dependency = if platform_features.is_empty() {
        format!("{}.workspace = true", platform.package())
    } else {
        let features = platform_features
            .iter()
            .map(|feature| format!("{feature:?}"))
            .collect::<Vec<_>>()
            .join(", ");
        format!(
            "{} = {{ workspace = true, features = [{}] }}",
            platform.package(),
            features
        )
    };
    let mut board_dependencies = Vec::new();
    if board.has_hardware_surface() {
        let catalog = load_catalog(workspace_root)?;
        let resolved = resolve_board(board, &catalog)?;
        validate_platform_hal(board, &resolved, &platform)?;
        for peripheral in resolved.peripherals() {
            let dependency = format!(
                "{}.workspace = true",
                peripheral.driver().implementation().package()
            );
            if !board_dependencies.contains(&dependency) {
                board_dependencies.push(dependency);
            }
        }
        board_dependencies.push(String::from("barracuda-driver.workspace = true"));
    }
    board_dependencies.sort();
    replace_file_block(
        &workspace_root.join("platforms/selected/Cargo.toml"),
        "# BEGIN GENERATED SELECTED PLATFORM",
        "# END GENERATED SELECTED PLATFORM",
        &platform_dependency,
    )?;
    replace_file_block(
        &workspace_root.join("boards/selected/Cargo.toml"),
        "# BEGIN GENERATED SELECTED BOARD HAL",
        "# END GENERATED SELECTED BOARD HAL",
        &board_dependencies.join("\n"),
    )?;

    let host = barracuda_platform_tool::host_tuple(workspace_root)?;
    let target = board
        .toolchain()
        .map_or(host.as_str(), |toolchain| toolchain.target());
    let mut cargo = format!(
        "# Generated by `cargo board select`; do not edit.\n\n[build]\ntarget = {target:?}\n"
    );
    if platform.application().launcher().is_some() {
        let runner = install_runner(workspace_root, &host)?;
        cargo.push_str(&format!(
            "\n[target.{target}]\nrunner = [{:?}, \"__run\", {:?}, \"--\"]\n",
            runner.to_string_lossy(),
            platform.name()
        ));
    }
    write_file(&workspace_root.join(".barracuda/cargo.toml"), &cargo)?;
    write_file(
        &workspace_root.join(".barracuda/selected-platform"),
        &format!("{}\n", platform.name()),
    )
}

fn install_runner(workspace_root: &Path, host: &str) -> Result<PathBuf, CommandError> {
    let executable = env::current_exe().map_err(|source| CommandError::RunnerInstall {
        path: PathBuf::from("current executable"),
        source,
    })?;
    let workspace_root =
        workspace_root
            .canonicalize()
            .map_err(|source| CommandError::RunnerInstall {
                path: workspace_root.to_owned(),
                source,
            })?;
    let runner = workspace_root
        .join(".barracuda/bin")
        .join(format!("barracuda-runner{}", env::consts::EXE_SUFFIX));
    let parent = runner.parent().unwrap_or(&workspace_root);
    fs::create_dir_all(parent).map_err(|source| CommandError::RunnerInstall {
        path: parent.to_owned(),
        source,
    })?;
    let temporary = runner.with_extension(format!("{host}.new"));
    fs::copy(&executable, &temporary).map_err(|source| CommandError::RunnerInstall {
        path: temporary.clone(),
        source,
    })?;
    fs::rename(&temporary, &runner).map_err(|source| CommandError::RunnerInstall {
        path: runner.clone(),
        source,
    })?;
    Ok(runner)
}

fn replace_file_block(path: &Path, begin: &str, end: &str, body: &str) -> Result<(), CommandError> {
    let old = fs::read_to_string(path).map_err(|source| CommandError::Read {
        path: path.to_owned(),
        source,
    })?;
    let new = replace_block(&old, begin, end, body)?;
    write_file(path, &new)
}

fn write_file(path: &Path, contents: &str) -> Result<(), CommandError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|source| CommandError::Read {
            path: parent.to_owned(),
            source,
        })?;
    }
    fs::write(path, contents).map_err(|source| CommandError::Read {
        path: path.to_owned(),
        source,
    })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use std::{cell::Cell, fs, path::Path};

    use barracuda_board_config::{read_selected_board, write_selected_board};
    use tempfile::tempdir;

    use super::{
        requires_platform_launcher, run_with_selector, sync_with_report, CommandError, SyncStatus,
    };

    #[test]
    fn target_runner_launches_only_the_system_application_through_platform() {
        assert!(requires_platform_launcher(Path::new("barracuda-system")));
        assert!(!requires_platform_launcher(Path::new("barracuda")));
        assert!(!requires_platform_launcher(Path::new(
            "channel_boundary-test"
        )));
    }

    fn add_board(root: &Path, name: &str) {
        let directory = root.join("boards/configs").join(name);
        fs::create_dir_all(&directory).expect("Board directory");
        fs::write(
            directory.join("board.yml"),
            format!(
                "name: {name}\nhardware:\n  chip: test\nnative-layout:\n  artifact: layout.yml\n"
            ),
        )
        .expect("Board YAML");
        fs::write(directory.join("layout.yml"), "layout\n").expect("native layout");
        add_selection_files(root);
    }

    fn add_selection_files(root: &Path) {
        let platform = root.join("platforms/test");
        fs::create_dir_all(&platform).expect("Platform directory");
        fs::write(
            platform.join("platform.yml"),
            "name: test\npackage: barracuda-platform-test\ncrate: barracuda_platform_test\ntype: TestPlatform\nselection:\n  board-chips: [test]\n  targets:\n    - os: test\nsystem-image:\n  layout:\n    driver: file-regions\n  flash:\n    driver: file\n    state-directory: .state\n    flash-image: flash.bin\n",
        )
        .expect("Platform manifest");
        fs::create_dir_all(root.join("platforms/selected")).expect("selected Platform directory");
        fs::write(
            root.join("platforms/selected/Cargo.toml"),
            "# BEGIN GENERATED SELECTED PLATFORM\nold\n# END GENERATED SELECTED PLATFORM\n",
        )
        .expect("selected Platform manifest");
        fs::create_dir_all(root.join("boards/selected")).expect("selected Board directory");
        fs::write(
            root.join("boards/selected/Cargo.toml"),
            "# BEGIN GENERATED SELECTED BOARD HAL\nold\n# END GENERATED SELECTED BOARD HAL\n",
        )
        .expect("selected Board manifest");
    }

    fn add_catalog_board(root: &Path, name: &str) {
        let directory = root.join("boards/configs").join(name);
        fs::create_dir_all(&directory).expect("Board directory");
        fs::write(
            directory.join("board.yml"),
            format!(
                "name: {name}\nhardware:\n  chip: {name}\nnative-layout:\n  artifact: layout.yml\n"
            ),
        )
        .expect("Board YAML");
        fs::write(directory.join("layout.yml"), "layout\n").expect("native layout");
    }

    fn add_driver(root: &Path, name: &str) {
        let driver = root.join("drivers").join(name);
        let crate_name = name.replace('-', "_");
        fs::create_dir_all(&driver).expect("Driver directory");
        fs::write(
            driver.join("driver.yml"),
            format!(
                "id: {name}\napi-version: 1\ncapability: test\nimplementation:\n  package: barracuda-{name}\n  crate: barracuda_{crate_name}\n  factory: \"::{{{{crate}}}}::Driver\"\n  bindings-expression: \"()\"\n  config-expression: \"()\"\nbindings: {{}}\nparameters: {{}}\n"
            ),
        )
        .expect("Driver manifest");
    }

    fn add_workspace(root: &Path) {
        fs::create_dir_all(root.join("drivers")).expect("Driver catalog");
        fs::write(
            root.join("Cargo.toml"),
            "[workspace.dependencies]\n# BEGIN GENERATED DRIVER WORKSPACE DEPENDENCIES\nold\n# END GENERATED DRIVER WORKSPACE DEPENDENCIES\n",
        )
        .expect("workspace manifest");
    }

    #[test]
    fn sync_discovers_drivers_and_validates_boards() {
        let root = tempdir().expect("temporary workspace");
        add_workspace(root.path());
        add_catalog_board(root.path(), "zeta-board");
        add_catalog_board(root.path(), "alpha-board");
        add_driver(root.path(), "zeta-driver");
        add_driver(root.path(), "alpha-driver");

        let report = sync_with_report(root.path(), false).expect("Board sync");

        assert_eq!(report.status(), SyncStatus::Updated);
        assert_eq!(report.boards(), 2);
        assert_eq!(report.drivers(), 2);
        let workspace =
            fs::read_to_string(root.path().join("Cargo.toml")).expect("workspace manifest");
        let alpha = workspace
            .find("barracuda-alpha-driver = { path = \"drivers/alpha-driver\" }")
            .expect("alpha Driver");
        let zeta = workspace
            .find("barracuda-zeta-driver = { path = \"drivers/zeta-driver\" }")
            .expect("zeta Driver");
        assert!(alpha < zeta);
    }

    #[test]
    fn board_sync_check_rejects_stale_registry_without_writing() {
        let root = tempdir().expect("temporary workspace");
        add_workspace(root.path());
        add_catalog_board(root.path(), "alpha-board");
        add_driver(root.path(), "alpha-driver");
        let before = fs::read_to_string(root.path().join("Cargo.toml")).expect("before");

        let error = sync_with_report(root.path(), true).expect_err("stale registry");

        assert!(matches!(error, CommandError::StaleRegistry));
        assert_eq!(
            fs::read_to_string(root.path().join("Cargo.toml")).expect("after"),
            before
        );
    }

    #[test]
    fn board_sync_rejects_an_invalid_driver_manifest() {
        let root = tempdir().expect("temporary workspace");
        add_workspace(root.path());
        add_catalog_board(root.path(), "alpha-board");
        add_driver(root.path(), "alpha-driver");
        fs::write(
            root.path().join("drivers/alpha-driver/driver.yml"),
            "id: wrong-id\n",
        )
        .expect("replace Driver manifest");

        let error = sync_with_report(root.path(), false).expect_err("invalid Driver manifest");

        assert!(matches!(error, CommandError::DriverCatalog(_)));
    }

    #[test]
    fn board_sync_rejects_a_hardware_surface_missing_from_the_platform_hal() {
        let root = tempdir().expect("temporary workspace");
        add_workspace(root.path());
        let platform = root.path().join("platforms/test");
        fs::create_dir_all(&platform).expect("Platform directory");
        fs::write(
            platform.join("platform.yml"),
            "name: test\npackage: test-platform\ncrate: test_platform\ntype: TestPlatform\nselection:\n  board-chips: [test]\n  targets:\n    - os: test\nsystem-image:\n  layout:\n    driver: file-regions\n  flash:\n    driver: file\n    state-directory: .state\n    flash-image: flash.bin\n",
        )
        .expect("Platform manifest");
        let directory = root.path().join("boards/configs/alpha-board");
        fs::create_dir_all(&directory).expect("Board directory");
        fs::write(
            directory.join("board.yml"),
            "name: alpha-board\nhardware:\n  chip: test\nnative-layout:\n  artifact: layout.yml\nexposed-io:\n  pins:\n    button:\n      pin: P0\n",
        )
        .expect("Board YAML");
        fs::write(directory.join("layout.yml"), "layout\n").expect("native layout");

        let error = sync_with_report(root.path(), false).expect_err("missing Platform HAL binding");

        assert!(matches!(error, CommandError::DriverGeneration(_)));
    }

    #[test]
    fn interactive_select_lists_boards_in_name_order_and_persists_the_choice() {
        let root = tempdir().expect("temporary workspace");
        add_board(root.path(), "stm32f429zi-nucleo");
        add_board(root.path(), "esp32c6-devkitc-1");
        fs::write(root.path().join("boards/configs/README.txt"), "not a Board")
            .expect("non-Board file");
        let mut output = Vec::new();

        run_with_selector(["select"], root.path(), &mut output, |boards, default| {
            assert_eq!(boards, ["esp32c6-devkitc-1", "stm32f429zi-nucleo"]);
            assert_eq!(default, None);
            Ok(Some(1))
        })
        .expect("interactive selection");

        assert_eq!(
            read_selected_board(root.path()).expect("read selection"),
            Some(String::from("stm32f429zi-nucleo"))
        );
    }

    #[test]
    fn interactive_select_highlights_the_current_board_by_default() {
        let root = tempdir().expect("temporary workspace");
        add_board(root.path(), "local-linux");
        add_board(root.path(), "local-macos");
        write_selected_board(root.path(), "local-macos").expect("current Board");

        run_with_selector(
            ["select"],
            root.path(),
            &mut Vec::new(),
            |boards, default| {
                assert_eq!(boards, ["local-linux", "local-macos"]);
                assert_eq!(default, Some(1));
                Ok(Some(0))
            },
        )
        .expect("interactive selection");

        assert_eq!(
            read_selected_board(root.path()).expect("read selection"),
            Some(String::from("local-linux"))
        );
    }

    #[test]
    fn interactive_select_rejects_an_empty_board_catalog() {
        let root = tempdir().expect("temporary workspace");
        fs::create_dir_all(root.path().join("boards/configs")).expect("configs directory");
        let prompted = Cell::new(false);

        let error = run_with_selector(
            ["select"],
            root.path(),
            &mut Vec::new(),
            |_boards, _default| {
                prompted.set(true);
                Ok(Some(0))
            },
        )
        .expect_err("empty Board catalog");

        assert!(!prompted.get());
        assert!(error.to_string().contains("no Board bundles found"));
    }

    #[test]
    fn explicit_board_name_does_not_open_the_interactive_prompt() {
        let root = tempdir().expect("temporary workspace");
        add_board(root.path(), "local-macos");
        let prompted = Cell::new(false);

        run_with_selector(
            ["select", "local-macos"],
            root.path(),
            &mut Vec::new(),
            |_boards, _default| {
                prompted.set(true);
                Ok(Some(0))
            },
        )
        .expect("explicit selection");

        assert!(!prompted.get());
    }

    #[test]
    fn cancelling_interactive_select_preserves_the_current_board() {
        let root = tempdir().expect("temporary workspace");
        add_board(root.path(), "local-linux");
        add_board(root.path(), "local-macos");
        write_selected_board(root.path(), "local-macos").expect("current Board");
        let mut output = Vec::new();

        run_with_selector(["select"], root.path(), &mut output, |_boards, _default| {
            Ok(None)
        })
        .expect("cancel selection");

        assert_eq!(
            read_selected_board(root.path()).expect("read selection"),
            Some(String::from("local-macos"))
        );
        assert_eq!(
            String::from_utf8(output).expect("UTF-8 output"),
            "Board selection cancelled.\n"
        );
    }
}
