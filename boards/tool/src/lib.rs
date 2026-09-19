//! Board selection command support.

use std::{
    env,
    ffi::OsString,
    fs,
    io::{self, Write},
    path::{Path, PathBuf},
    process::{Command as ProcessCommand, Stdio},
};

use barracuda_board_config::{
    parse, read_selected_board, validate_board_name, write_selected_board, BoardDefinition,
    ConfigError, SelectionError,
};
use barracuda_peripheral_config::{
    load_catalog, render_board_hal_for_platform, resolve_board, validate_platform_hal,
    CatalogError, GenerateError, ResolveError as PeripheralResolveError,
};
use barracuda_platform_config::{resolve_board_platform, ResolveError as PlatformResolveError};
use clap::{Parser, Subcommand};
use dialoguer::{theme::ColorfulTheme, FuzzySelect};

mod selection_packages;

const PERIPHERAL_WORKSPACE_BEGIN: &str =
    "# BEGIN GENERATED PERIPHERAL IMPLEMENTATION WORKSPACE DEPENDENCIES";
const PERIPHERAL_WORKSPACE_END: &str =
    "# END GENERATED PERIPHERAL IMPLEMENTATION WORKSPACE DEPENDENCIES";

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
    implementations: usize,
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

    /// Returns the number of discovered peripheral implementation manifests.
    #[must_use]
    pub const fn implementations(&self) -> usize {
        self.implementations
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
    /// Validate Boards and synchronize peripheral implementations into the workspace registry.
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
        /// Arguments inserted before user arguments when invoking the Platform launcher.
        #[arg(long = "launcher-argument", hide = true, action = clap::ArgAction::Append)]
        launcher_arguments: Vec<OsString>,
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
    /// The peripheral implementation catalog is invalid or unavailable.
    #[error(transparent)]
    PeripheralCatalog(#[from] CatalogError),
    /// A Board does not satisfy its selected peripheral implementation schemas.
    #[error(transparent)]
    PeripheralComposition(#[from] PeripheralResolveError),
    /// A resolved Board cannot be rendered into static Rust bindings.
    #[error(transparent)]
    PeripheralGeneration(#[from] GenerateError),
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
    /// The Platform's required rustup toolchain is not installed.
    #[error("required rustup toolchain `{toolchain}` is unavailable\n\n{prompt}")]
    ToolchainMissing {
        /// rustup toolchain selected by the Platform.
        toolchain: String,
        /// Platform-owned installation guidance.
        prompt: String,
    },
    /// rustup could not be started while applying the Platform build policy.
    #[error("failed to run rustup for toolchain `{toolchain}`: {source}\n\n{prompt}")]
    ToolchainStart {
        /// rustup toolchain selected by the Platform.
        toolchain: String,
        /// Platform-owned installation guidance.
        prompt: String,
        /// Underlying process error.
        #[source]
        source: io::Error,
    },
    /// rustup could not activate the Platform's toolchain for this workspace.
    #[error("rustup could not activate toolchain `{toolchain}` for `{path}`")]
    ToolchainActivation {
        /// rustup toolchain selected by the Platform.
        toolchain: String,
        /// Workspace that should receive the override.
        path: PathBuf,
    },
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
                "{}{action} {} registry ({} Boards, {} peripheral implementations).",
                prefix(color, barracuda_cli_style::SUCCESS, "✔"),
                styled(color, barracuda_cli_style::EMPHASIS, "Board"),
                report.boards(),
                report.implementations()
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
            mut launcher_arguments,
            application,
            arguments,
        } => {
            launcher_arguments.extend(arguments);
            run_selected_application(workspace_root, &platform, &application, &launcher_arguments)
        }
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

/// Validates discovered Boards and synchronizes peripheral implementations into the workspace registry.
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
    let implementation_count = catalog.implementations().count();

    let manifest_path = workspace_root.join("Cargo.toml");
    let old_manifest = fs::read_to_string(&manifest_path).map_err(|source| CommandError::Read {
        path: manifest_path.clone(),
        source,
    })?;
    let implementation_dependencies = catalog
        .implementations()
        .map(|implementation| {
            let directory = implementation
                .directory()
                .strip_prefix(workspace_root)
                .unwrap_or_else(|_| implementation.directory());
            format!(
                "{} = {{ path = {:?} }}",
                implementation.implementation().package(),
                directory.to_string_lossy()
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    let manifest = replace_block(
        &old_manifest,
        PERIPHERAL_WORKSPACE_BEGIN,
        PERIPHERAL_WORKSPACE_END,
        &implementation_dependencies,
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
        implementations: implementation_count,
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
    select_board_with_toolchain(
        workspace_root,
        name,
        output,
        color,
        activate_rustup_toolchain,
    )
}

fn select_board_with_toolchain<W, F>(
    workspace_root: &Path,
    name: &str,
    output: &mut W,
    color: bool,
    activate_toolchain: F,
) -> Result<(), CommandError>
where
    W: Write,
    F: FnOnce(&Path, &str, &str) -> Result<(), CommandError>,
{
    let bundle = workspace_root.join("boards/configs").join(name);
    let board = read_board(workspace_root, name)?;
    let native_layout = bundle.join(board.native_layout().artifact());
    if !native_layout.is_file() {
        return Err(CommandError::NativeLayoutMissing {
            name: name.to_owned(),
            path: native_layout,
        });
    }
    let platform = resolve_board_platform(
        workspace_root,
        board.hardware().chip(),
        board.toolchain().map(|toolchain| toolchain.target()),
    )?;
    if let Some(toolchain) = platform.build().rustup_toolchain() {
        let prompt = platform
            .build()
            .missing_toolchain_prompt()
            .unwrap_or_default();
        activate_toolchain(workspace_root, toolchain, prompt)?;
    }

    write_selected_build(workspace_root, &board, &platform)?;
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

fn activate_rustup_toolchain(
    workspace_root: &Path,
    toolchain: &str,
    prompt: &str,
) -> Result<(), CommandError> {
    let workspace_root = workspace_root
        .canonicalize()
        .map_err(|source| CommandError::Read {
            path: workspace_root.to_owned(),
            source,
        })?;
    let available = ProcessCommand::new("rustup")
        .args(["run", toolchain, "rustc", "--version"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map_err(|source| CommandError::ToolchainStart {
            toolchain: toolchain.to_owned(),
            prompt: prompt.to_owned(),
            source,
        })?;
    if !available.success() {
        return Err(CommandError::ToolchainMissing {
            toolchain: toolchain.to_owned(),
            prompt: prompt.to_owned(),
        });
    }
    let activated = ProcessCommand::new("rustup")
        .args(["override", "set", toolchain, "--path"])
        .arg(&workspace_root)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map_err(|source| CommandError::ToolchainStart {
            toolchain: toolchain.to_owned(),
            prompt: prompt.to_owned(),
            source,
        })?;
    if activated.success() {
        Ok(())
    } else {
        Err(CommandError::ToolchainActivation {
            toolchain: toolchain.to_owned(),
            path: workspace_root,
        })
    }
}

fn write_selected_build(
    workspace_root: &Path,
    board: &BoardDefinition,
    platform: &barracuda_platform_config::PlatformDefinition,
) -> Result<(), CommandError> {
    let platform_features = platform.cargo_features_for_chip(board.hardware().chip());
    let mut board_dependencies = Vec::new();
    if board.has_hardware_surface() {
        let catalog = load_catalog(workspace_root)?;
        let resolved = resolve_board(board, &catalog)?;
        validate_platform_hal(board, &resolved, platform)?;
        for peripheral in resolved.peripherals() {
            let dependency = peripheral
                .implementation()
                .implementation()
                .package()
                .to_owned();
            if !board_dependencies.contains(&dependency) {
                board_dependencies.push(dependency);
            }
        }
    }
    board_dependencies.sort();
    selection_packages::write(
        workspace_root,
        Some(platform.package()),
        platform_features,
        &board_dependencies,
    )
    .map_err(|source| {
        CommandError::Generated(format!(
            "failed to write local selection packages: {source}"
        ))
    })?;

    let host = barracuda_platform_tool::host_tuple(workspace_root)?;
    let target = board
        .toolchain()
        .map_or(host.as_str(), |toolchain| toolchain.target());
    let mut cargo = format!(
        "# Generated by `cargo board select`; do not edit.\n\n[build]\ntarget = {target:?}\n"
    );
    if !platform.build().build_std().is_empty() {
        let crates = platform
            .build()
            .build_std()
            .iter()
            .map(|crate_name| format!("{crate_name:?}"))
            .collect::<Vec<_>>()
            .join(", ");
        cargo.push_str(&format!("\n[unstable]\nbuild-std = [{crates}]\n"));
    }
    let mut has_environment = false;
    if let Some(environment_file) = platform.build().environment_file() {
        for (name, value) in read_environment_file(workspace_root, environment_file)? {
            append_cargo_environment(
                &mut cargo,
                &mut has_environment,
                &name,
                &format!("{{ value = {value:?}, force = true }}"),
            );
        }
    }
    if target == "riscv32imafc-unknown-none-elf" {
        for (name, value) in [
            (
                "CC_riscv32imafc_unknown_none_elf",
                "{ value = \"boards/tool/assets/riscv32-esp-elf-gcc\", relative = true }",
            ),
            (
                "CXX_riscv32imafc_unknown_none_elf",
                "{ value = \"boards/tool/assets/riscv32-esp-elf-g++\", relative = true }",
            ),
            (
                "AR_riscv32imafc_unknown_none_elf",
                "{ value = \"boards/tool/assets/riscv32-esp-elf-ar\", relative = true }",
            ),
            (
                "AR",
                "{ value = \"boards/tool/assets/riscv32-esp-elf-ar\", relative = true, force = true }",
            ),
            (
                "RANLIB",
                "{ value = \"boards/tool/assets/riscv32-esp-elf-ranlib\", relative = true, force = true }",
            ),
            (
                "CMAKE_riscv32imafc_unknown_none_elf",
                "{ value = \"boards/tool/assets/esp32p4-cmake\", relative = true }",
            ),
            (
                "CMAKE_TOOLCHAIN_FILE_riscv32imafc_unknown_none_elf",
                "{ value = \"boards/tool/assets/esp32p4-toolchain.cmake\", relative = true }",
            ),
            (
                "CFLAGS_riscv32imafc_unknown_none_elf",
                "\"-march=rv32imafc -mabi=ilp32f -DBARRACUDA_ESP32P4_HARD_FLOAT=4\"",
            ),
        ] {
            append_cargo_environment(&mut cargo, &mut has_environment, name, value);
        }
        cargo.push_str(&format!(
            "\n[target.{target}]\nlinker = \"boards/tool/assets/esp32p4-linker\"\nrustflags = [\"-C\", \"link-arg=-Tlinkall.x\"]\n"
        ));
    }
    if let Some(tool_prefix) = target
        .strip_suffix("-none-elf")
        .filter(|target| target.starts_with("xtensa-esp32"))
    {
        let environment_suffix = target.replace('-', "_");
        let bindgen_include = workspace_root.join("boards/tool/assets/xtensa-include");
        for (name, value) in [
            (
                format!("CC_{environment_suffix}"),
                format!("{tool_prefix}-elf-gcc"),
            ),
            (
                format!("AR_{environment_suffix}"),
                format!("{tool_prefix}-elf-ar"),
            ),
            (
                format!("CFLAGS_{environment_suffix}"),
                String::from("-mlongcalls"),
            ),
            (
                format!("BINDGEN_EXTRA_CLANG_ARGS_{environment_suffix}"),
                format!(
                    "--target=xtensa-esp-elf -I{:?}",
                    bindgen_include.to_string_lossy()
                ),
            ),
        ] {
            append_cargo_environment(
                &mut cargo,
                &mut has_environment,
                &name,
                &format!("{value:?}"),
            );
        }
        cargo.push_str(&format!(
            "\n[target.{target}]\nrustflags = [\"-C\", \"link-arg=-Tlinkall.x\", \"-C\", \"link-arg=-Wl,--allow-multiple-definition\", \"-C\", \"link-arg=-Wl,--start-group\", \"-C\", \"link-arg=-lc\", \"-C\", \"link-arg=-lm\", \"-C\", \"link-arg=-lgcc\", \"-C\", \"link-arg=-Wl,--end-group\"]\n"
        ));
    }
    if platform.application().launcher().is_some() {
        let runner = install_runner(workspace_root, &host)?;
        if target != "riscv32imafc-unknown-none-elf" && !target.starts_with("xtensa-esp32") {
            cargo.push_str(&format!("\n[target.{target}]\n"));
        }
        let mut runner_arguments = format!(
            "{:?}, \"__run\", {:?}",
            runner.to_string_lossy(),
            platform.name()
        );
        if target == "riscv32imafc-unknown-none-elf" || target.starts_with("xtensa-esp32") {
            if let Some(flash_size) = board.hardware().flash_size() {
                append_launcher_argument(&mut runner_arguments, "--flash-size");
                append_launcher_argument(&mut runner_arguments, flash_size);
            }
            let layout = Path::new("boards/configs")
                .join(board.name())
                .join(board.native_layout().artifact());
            append_launcher_argument(&mut runner_arguments, "--partition-table");
            append_launcher_argument(&mut runner_arguments, &layout.to_string_lossy());
            append_launcher_argument(&mut runner_arguments, "--target-app-partition");
            append_launcher_argument(&mut runner_arguments, "ota_0");
        }
        cargo.push_str(&format!("runner = [{runner_arguments}]\n"));
    }
    write_file(&workspace_root.join(".barracuda/cargo.toml"), &cargo)?;
    write_file(
        &workspace_root.join(".barracuda/selected-platform"),
        &format!("{}\n", platform.name()),
    )
}

fn append_cargo_environment(
    cargo: &mut String,
    has_environment: &mut bool,
    name: &str,
    value: &str,
) {
    if !*has_environment {
        cargo.push_str("\n[env]\n");
        *has_environment = true;
    }
    cargo.push_str(&format!("{name} = {value}\n"));
}

fn read_environment_file(
    workspace_root: &Path,
    configured_path: &Path,
) -> Result<Vec<(String, String)>, CommandError> {
    let path = expand_environment_path(workspace_root, configured_path);
    let contents = match fs::read_to_string(&path) {
        Ok(contents) => contents,
        Err(source) if source.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(source) => {
            return Err(CommandError::Read {
                path: path.clone(),
                source,
            });
        }
    };
    let mut environment = Vec::new();
    for line in contents.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let assignment = line.strip_prefix("export ").ok_or_else(|| {
            CommandError::Generated(format!(
                "toolchain environment `{}` contains an unsupported line: `{line}`",
                path.display()
            ))
        })?;
        let (name, value) = assignment.split_once('=').ok_or_else(|| {
            CommandError::Generated(format!(
                "toolchain environment `{}` contains an invalid export: `{line}`",
                path.display()
            ))
        })?;
        if name.is_empty()
            || !name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
        {
            return Err(CommandError::Generated(format!(
                "toolchain environment `{}` contains an invalid variable: `{name}`",
                path.display()
            )));
        }
        let value = value.trim();
        let value = value
            .strip_prefix('"')
            .and_then(|value| value.strip_suffix('"'))
            .or_else(|| {
                value
                    .strip_prefix('\'')
                    .and_then(|value| value.strip_suffix('\''))
            })
            .unwrap_or(value);
        environment.push((name.to_owned(), expand_environment_value(value)));
    }
    Ok(environment)
}

fn expand_environment_path(workspace_root: &Path, configured_path: &Path) -> PathBuf {
    let configured = configured_path.to_string_lossy();
    if let Some(relative) = configured.strip_prefix("~/") {
        return env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| workspace_root.to_owned())
            .join(relative);
    }
    if configured_path.is_absolute() {
        configured_path.to_owned()
    } else {
        workspace_root.join(configured_path)
    }
}

fn expand_environment_value(value: &str) -> String {
    let mut expanded = String::new();
    let mut characters = value.chars().peekable();
    while let Some(character) = characters.next() {
        if character != '$' {
            expanded.push(character);
            continue;
        }
        let braced = characters.next_if_eq(&'{').is_some();
        let mut name = String::new();
        while characters
            .peek()
            .is_some_and(|character| character.is_ascii_alphanumeric() || *character == '_')
        {
            if let Some(character) = characters.next() {
                name.push(character);
            }
        }
        if braced && characters.next_if_eq(&'}').is_none() {
            expanded.push_str("${");
            expanded.push_str(&name);
            continue;
        }
        if name.is_empty() {
            expanded.push('$');
            if braced {
                expanded.push('{');
            }
            continue;
        }
        if let Some(current) = env::var_os(&name) {
            expanded.push_str(&current.to_string_lossy());
        }
    }
    expanded
}

fn append_launcher_argument(runner: &mut String, argument: &str) {
    runner.push_str(&format!(
        ", {:?}",
        format!("--launcher-argument={argument}")
    ));
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
        requires_platform_launcher, run_with_selector, select_board_with_toolchain,
        sync_with_report, CommandError, SyncStatus,
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
            platform.join("Cargo.toml"),
            "[package]\nname = \"barracuda-platform-test\"\nversion = \"0.1.0\"\n",
        )
        .expect("Platform Cargo manifest");
        fs::write(
            platform.join("platform.yml"),
            "name: test\ninfo:\n  family: test\n  environment: hosted\npackage: barracuda-platform-test\ncrate: barracuda_platform_test\ntype: TestPlatform\nselection:\n  board-chips: [test]\n  targets:\n    - os: test\nsystem-image:\n  layout:\n    driver: file-regions\n  flash:\n    driver: file\n    state-directory: .state\n    flash-image: flash.bin\n",
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

    #[test]
    fn selection_activates_the_platform_toolchain_and_configures_build_std() {
        let root = tempdir().expect("temporary workspace");
        add_board(root.path(), "device");
        let manifest = root.path().join("platforms/test/platform.yml");
        let yaml = fs::read_to_string(&manifest).expect("Platform manifest");
        fs::write(
            &manifest,
            yaml.replace(
                "selection:\n",
                "build:\n  rustup-toolchain: vendor\n  build-std: [core, alloc]\n  environment-file: vendor.env\n  missing-toolchain-prompt: |\n    Install the vendor toolchain.\nselection:\n",
            ),
        )
        .expect("Platform build policy");
        fs::write(
            root.path().join("vendor.env"),
            "export LIBRARY_PATH=\"/opt/vendor/lib\"\nexport PATH=\"/opt/vendor/bin:$PATH\"\n",
        )
        .expect("toolchain environment");
        let activated = Cell::new(false);

        select_board_with_toolchain(
            root.path(),
            "device",
            &mut Vec::new(),
            false,
            |workspace, toolchain, prompt| {
                assert_eq!(workspace, root.path());
                assert_eq!(toolchain, "vendor");
                assert_eq!(prompt, "Install the vendor toolchain.\n");
                activated.set(true);
                Ok(())
            },
        )
        .expect("select Board");

        assert!(activated.get());
        let cargo = fs::read_to_string(root.path().join(".barracuda/cargo.toml"))
            .expect("local Cargo selection");
        assert!(cargo.contains("[unstable]"));
        assert!(cargo.contains("build-std = [\"core\", \"alloc\"]"));
        assert!(cargo.contains("LIBRARY_PATH = { value = \"/opt/vendor/lib\", force = true }"));
        assert!(cargo.contains("PATH = { value = \"/opt/vendor/bin:"));
        assert!(!cargo.contains("$PATH"));
    }

    #[test]
    fn missing_toolchain_prompt_prevents_selection_changes() {
        let root = tempdir().expect("temporary workspace");
        add_board(root.path(), "device");
        let manifest = root.path().join("platforms/test/platform.yml");
        let yaml = fs::read_to_string(&manifest).expect("Platform manifest");
        fs::write(
            &manifest,
            yaml.replace(
                "selection:\n",
                "build:\n  rustup-toolchain: vendor\n  missing-toolchain-prompt: |\n    Install the vendor toolchain.\nselection:\n",
            ),
        )
        .expect("Platform build policy");

        let error = select_board_with_toolchain(
            root.path(),
            "device",
            &mut Vec::new(),
            false,
            |_workspace, toolchain, prompt| {
                Err(CommandError::ToolchainMissing {
                    toolchain: toolchain.to_owned(),
                    prompt: prompt.to_owned(),
                })
            },
        )
        .expect_err("missing toolchain must stop selection");

        assert!(error.to_string().contains("Install the vendor toolchain."));
        assert_eq!(
            read_selected_board(root.path()).expect("read selection"),
            None
        );
        assert!(!root.path().join(".barracuda/cargo.toml").exists());
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

    fn add_implementation(root: &Path, name: &str) {
        let driver = root.join("peripherals/impl/indicator").join(name);
        let crate_name = name.replace('-', "_");
        fs::create_dir_all(&driver).expect("peripheral implementation directory");
        fs::write(
            driver.join("peripheral.yml"),
            format!(
                "id: {name}\napi-version: 1\nperipheral: indicator\nimplementation:\n  package: barracuda-{name}\n  crate: barracuda_{crate_name}\n  factory: \"::{{{{crate}}}}::Implementation\"\n  bindings-expression: \"()\"\n  config-expression: \"()\"\nbindings: {{}}\nparameters: {{}}\n"
            ),
        )
        .expect("peripheral implementation manifest");
    }

    fn add_workspace(root: &Path) {
        fs::create_dir_all(root.join("peripherals/impl"))
            .expect("peripheral implementation catalog");
        fs::write(
            root.join("Cargo.toml"),
            "[workspace.dependencies]\n# BEGIN GENERATED PERIPHERAL IMPLEMENTATION WORKSPACE DEPENDENCIES\nold\n# END GENERATED PERIPHERAL IMPLEMENTATION WORKSPACE DEPENDENCIES\n",
        )
        .expect("workspace manifest");
    }

    #[test]
    fn sync_discovers_implementations_and_validates_boards() {
        let root = tempdir().expect("temporary workspace");
        add_workspace(root.path());
        add_catalog_board(root.path(), "zeta-board");
        add_catalog_board(root.path(), "alpha-board");
        add_implementation(root.path(), "zeta-driver");
        add_implementation(root.path(), "alpha-driver");

        let report = sync_with_report(root.path(), false).expect("Board sync");

        assert_eq!(report.status(), SyncStatus::Updated);
        assert_eq!(report.boards(), 2);
        assert_eq!(report.implementations(), 2);
        let workspace =
            fs::read_to_string(root.path().join("Cargo.toml")).expect("workspace manifest");
        let alpha = workspace
            .find("barracuda-alpha-driver = { path = \"peripherals/impl/indicator/alpha-driver\" }")
            .expect("alpha implementation");
        let zeta = workspace
            .find("barracuda-zeta-driver = { path = \"peripherals/impl/indicator/zeta-driver\" }")
            .expect("zeta implementation");
        assert!(alpha < zeta);
    }

    #[test]
    fn board_sync_check_rejects_stale_registry_without_writing() {
        let root = tempdir().expect("temporary workspace");
        add_workspace(root.path());
        add_catalog_board(root.path(), "alpha-board");
        add_implementation(root.path(), "alpha-driver");
        let before = fs::read_to_string(root.path().join("Cargo.toml")).expect("before");

        let error = sync_with_report(root.path(), true).expect_err("stale registry");

        assert!(matches!(error, CommandError::StaleRegistry));
        assert_eq!(
            fs::read_to_string(root.path().join("Cargo.toml")).expect("after"),
            before
        );
    }

    #[test]
    fn board_sync_rejects_an_invalid_implementation_manifest() {
        let root = tempdir().expect("temporary workspace");
        add_workspace(root.path());
        add_catalog_board(root.path(), "alpha-board");
        add_implementation(root.path(), "alpha-driver");
        fs::write(
            root.path()
                .join("peripherals/impl/indicator/alpha-driver/peripheral.yml"),
            "id: wrong-id\n",
        )
        .expect("replace peripheral implementation manifest");

        let error = sync_with_report(root.path(), false)
            .expect_err("invalid peripheral implementation manifest");

        assert!(matches!(error, CommandError::PeripheralCatalog(_)));
    }

    #[test]
    fn board_sync_rejects_a_hardware_surface_missing_from_the_platform_hal() {
        let root = tempdir().expect("temporary workspace");
        add_workspace(root.path());
        let platform = root.path().join("platforms/test");
        fs::create_dir_all(&platform).expect("Platform directory");
        fs::write(
            platform.join("platform.yml"),
            "name: test\ninfo:\n  family: test\n  environment: hosted\npackage: test-platform\ncrate: test_platform\ntype: TestPlatform\nselection:\n  board-chips: [test]\n  targets:\n    - os: test\nsystem-image:\n  layout:\n    driver: file-regions\n  flash:\n    driver: file\n    state-directory: .state\n    flash-image: flash.bin\n",
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

        assert!(matches!(error, CommandError::PeripheralGeneration(_)));
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
