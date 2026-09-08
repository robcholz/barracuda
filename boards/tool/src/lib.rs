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
use barracuda_platform_config::{resolve_board_platform, ResolveError};
use clap::{Parser, Subcommand};
use dialoguer::{theme::ColorfulTheme, FuzzySelect};

const WORKSPACE_BEGIN: &str = "# BEGIN GENERATED BOARD WORKSPACE DEPENDENCIES";
const WORKSPACE_END: &str = "# END GENERATED BOARD WORKSPACE DEPENDENCIES";

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
    board_hals: usize,
}

/// Convention-discovered Cargo dependency for one Board-owned HAL crate.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BoardHalDependency {
    package: String,
    path: PathBuf,
}

impl BoardHalDependency {
    /// Returns the conventional Board HAL package name.
    #[must_use]
    pub fn package(&self) -> &str {
        &self.package
    }

    /// Returns the Board HAL path relative to the workspace.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Returns the Rust crate identifier derived from the Cargo package name.
    #[must_use]
    pub fn crate_name(&self) -> String {
        self.package.replace('-', "_")
    }
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

    /// Returns the number of distinct registered Board HAL packages.
    #[must_use]
    pub const fn board_hals(&self) -> usize {
        self.board_hals
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
    /// Synchronize discovered Board HALs into the workspace registry.
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
    /// The Board's Platform could not be resolved.
    #[error(transparent)]
    Platform(#[from] ResolveError),
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
    /// A Board declares hardware but has no conventionally located HAL crate.
    #[error("Board `{name}` declares hardware resources but has no HAL at `{path}`")]
    BoardHalMissing {
        /// Board requiring a HAL.
        name: String,
        /// Expected HAL manifest path.
        path: PathBuf,
    },
    /// A conventionally located Board HAL has the wrong Cargo package name.
    #[error("Board `{name}` HAL package must be `{expected}`, found `{declared}`")]
    BoardHalPackageMismatch {
        /// Owning Board.
        name: String,
        /// Required package name.
        expected: String,
        /// Package name declared by the HAL manifest.
        declared: String,
    },
    /// A Board HAL Cargo manifest is malformed.
    #[error("invalid Board HAL manifest `{path}`: {source}")]
    BoardHalManifest {
        /// Manifest path.
        path: PathBuf,
        /// TOML parsing failure.
        #[source]
        source: toml::de::Error,
    },
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
                "{}{action} {} registry ({} Boards, {} HALs).",
                prefix(color, barracuda_cli_style::SUCCESS, "✔"),
                styled(color, barracuda_cli_style::EMPHASIS, "Board"),
                report.boards(),
                report.board_hals()
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

/// Synchronizes discovered Board HALs into the tracked workspace registry.
///
/// # Errors
///
/// Returns an error when Board discovery, generated markers, or file access fails.
pub fn sync_with_report(workspace_root: &Path, check: bool) -> Result<SyncReport, CommandError> {
    let boards = discover_boards(workspace_root)?;
    let mut board_hals = Vec::new();
    for name in &boards {
        let board = read_board(workspace_root, name)?;
        match board_hal_dependency(workspace_root, name)? {
            Some(board_hal) => board_hals.push(board_hal),
            None if board.has_hardware_surface() => {
                return Err(CommandError::BoardHalMissing {
                    name: name.clone(),
                    path: board_hal_manifest_path(workspace_root, name),
                });
            }
            None => {}
        }
    }

    let manifest_path = workspace_root.join("Cargo.toml");
    let old_manifest = fs::read_to_string(&manifest_path).map_err(|source| CommandError::Read {
        path: manifest_path.clone(),
        source,
    })?;
    let dependencies = board_hals
        .iter()
        .map(|board_hal| {
            format!(
                "{} = {{ path = {:?} }}",
                board_hal.package(),
                board_hal.path().to_string_lossy()
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    let manifest = replace_block(&old_manifest, WORKSPACE_BEGIN, WORKSPACE_END, &dependencies)?;
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
        board_hals: board_hals.len(),
    })
}

/// Discovers a Board HAL from `boards/configs/<board>/hal/Cargo.toml`.
///
/// # Errors
///
/// Returns an error when the Board name is invalid or the manifest is malformed
/// or violates the conventional `barracuda-board-<board>` package name.
pub fn board_hal_dependency(
    workspace_root: &Path,
    board_name: &str,
) -> Result<Option<BoardHalDependency>, CommandError> {
    validate_board_name(board_name)?;
    let manifest_path = board_hal_manifest_path(workspace_root, board_name);
    let contents = match fs::read_to_string(&manifest_path) {
        Ok(contents) => contents,
        Err(source) if source.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(source) => {
            return Err(CommandError::Read {
                path: manifest_path,
                source,
            });
        }
    };
    let manifest = toml::from_str::<toml::Value>(&contents).map_err(|source| {
        CommandError::BoardHalManifest {
            path: manifest_path.clone(),
            source,
        }
    })?;
    let declared = manifest
        .get("package")
        .and_then(|package| package.get("name"))
        .and_then(toml::Value::as_str)
        .unwrap_or("");
    let expected = format!("barracuda-board-{board_name}");
    if declared != expected {
        return Err(CommandError::BoardHalPackageMismatch {
            name: board_name.to_owned(),
            expected,
            declared: declared.to_owned(),
        });
    }
    Ok(Some(BoardHalDependency {
        package: expected,
        path: PathBuf::from("boards/configs").join(board_name).join("hal"),
    }))
}

fn board_hal_manifest_path(workspace_root: &Path, board_name: &str) -> PathBuf {
    workspace_root
        .join("boards/configs")
        .join(board_name)
        .join("hal/Cargo.toml")
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
    replace_file_block(
        &workspace_root.join("platforms/selected/Cargo.toml"),
        "# BEGIN GENERATED SELECTED PLATFORM",
        "# END GENERATED SELECTED PLATFORM",
        &platform_dependency,
    )?;
    let board_hal_dependency = match board_hal_dependency(workspace_root, board.name())? {
        Some(board_hal) => format!("{}.workspace = true", board_hal.package()),
        None if board.has_hardware_surface() => {
            return Err(CommandError::BoardHalMissing {
                name: board.name().to_owned(),
                path: board_hal_manifest_path(workspace_root, board.name()),
            });
        }
        None => String::new(),
    };
    replace_file_block(
        &workspace_root.join("boards/selected/Cargo.toml"),
        "# BEGIN GENERATED SELECTED BOARD HAL",
        "# END GENERATED SELECTED BOARD HAL",
        &board_hal_dependency,
    )?;

    let host = barracuda_platform_tool::host_tuple(workspace_root)?;
    let target = board
        .toolchain()
        .map_or(host.as_str(), |toolchain| toolchain.target());
    let mut cargo = format!(
        "# Generated by `cargo board select`; do not edit.\n\n[build]\ntarget = {target:?}\n"
    );
    let rustflags = platform.cargo_rustflags_for_target(target);
    if platform.application().launcher().is_some() || !rustflags.is_empty() {
        cargo.push_str(&format!("\n[target.{target}]\n"));
        if !rustflags.is_empty() {
            let rustflags = rustflags
                .iter()
                .map(|flag| format!("{flag:?}"))
                .collect::<Vec<_>>()
                .join(", ");
            cargo.push_str(&format!("rustflags = [{rustflags}]\n"));
        }
        if platform.application().launcher().is_some() {
            let runner = install_runner(workspace_root, &host)?;
            cargo.push_str(&format!(
                "runner = [{:?}, \"__run\", {:?}, \"--\"]\n",
                runner.to_string_lossy(),
                platform.name()
            ));
        }
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

    fn add_board_hal(root: &Path, name: &str) {
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
        let hal = directory.join("hal");
        fs::create_dir_all(&hal).expect("Board HAL directory");
        fs::write(
            hal.join("Cargo.toml"),
            format!("[package]\nname = \"barracuda-board-{name}\"\nversion = \"0.1.0\"\n"),
        )
        .expect("Board HAL manifest");
    }

    fn add_workspace(root: &Path) {
        fs::write(
            root.join("Cargo.toml"),
            "[workspace.dependencies]\n# BEGIN GENERATED BOARD WORKSPACE DEPENDENCIES\nold\n# END GENERATED BOARD WORKSPACE DEPENDENCIES\n",
        )
        .expect("workspace manifest");
    }

    #[test]
    fn sync_discovers_board_hals_from_their_bundle_directories() {
        let root = tempdir().expect("temporary workspace");
        add_workspace(root.path());
        add_board_hal(root.path(), "zeta-board");
        add_board_hal(root.path(), "alpha-board");

        let report = sync_with_report(root.path(), false).expect("Board sync");

        assert_eq!(report.status(), SyncStatus::Updated);
        assert_eq!(report.boards(), 2);
        assert_eq!(report.board_hals(), 2);
        let workspace =
            fs::read_to_string(root.path().join("Cargo.toml")).expect("workspace manifest");
        let alpha = workspace
            .find("barracuda-board-alpha-board = { path = \"boards/configs/alpha-board/hal\" }")
            .expect("alpha HAL");
        let zeta = workspace
            .find("barracuda-board-zeta-board = { path = \"boards/configs/zeta-board/hal\" }")
            .expect("zeta HAL");
        assert!(alpha < zeta);
    }

    #[test]
    fn board_sync_check_rejects_stale_registry_without_writing() {
        let root = tempdir().expect("temporary workspace");
        add_workspace(root.path());
        add_board_hal(root.path(), "alpha-board");
        let before = fs::read_to_string(root.path().join("Cargo.toml")).expect("before");

        let error = sync_with_report(root.path(), true).expect_err("stale registry");

        assert!(matches!(error, CommandError::StaleRegistry));
        assert_eq!(
            fs::read_to_string(root.path().join("Cargo.toml")).expect("after"),
            before
        );
    }

    #[test]
    fn board_sync_rejects_a_nonconventional_hal_package_name() {
        let root = tempdir().expect("temporary workspace");
        add_workspace(root.path());
        add_board_hal(root.path(), "alpha-board");
        fs::write(
            root.path()
                .join("boards/configs/alpha-board/hal/Cargo.toml"),
            "[package]\nname = \"custom-hal\"\nversion = \"0.1.0\"\n",
        )
        .expect("replace Board HAL manifest");

        let error = sync_with_report(root.path(), false).expect_err("invalid HAL package");

        assert!(matches!(
            error,
            CommandError::BoardHalPackageMismatch { .. }
        ));
    }

    #[test]
    fn board_sync_rejects_a_hardware_surface_without_a_hal_crate() {
        let root = tempdir().expect("temporary workspace");
        add_workspace(root.path());
        let directory = root.path().join("boards/configs/alpha-board");
        fs::create_dir_all(&directory).expect("Board directory");
        fs::write(
            directory.join("board.yml"),
            "name: alpha-board\nhardware:\n  chip: test\nnative-layout:\n  artifact: layout.yml\nexposed-io:\n  gpio:\n    button:\n      pin: P0\n",
        )
        .expect("Board YAML");
        fs::write(directory.join("layout.yml"), "layout\n").expect("native layout");

        let error = sync_with_report(root.path(), false).expect_err("missing Board HAL");

        assert!(matches!(error, CommandError::BoardHalMissing { .. }));
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
