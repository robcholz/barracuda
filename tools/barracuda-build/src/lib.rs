//! Generated-workspace preparation for Board-selected Barracuda builds.

use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};

use barracuda_board_config::{parse, read_selected_board};
use barracuda_platform_config::resolve_board_platform;
use barracuda_platform_config::ApplicationConfig;
use toml_edit::{Array, DocumentMut, InlineTable, Item, Value};

/// Selection resolved while preparing a generated build workspace.
#[derive(Debug, PartialEq, Eq)]
pub struct BuildSelection {
    board: String,
    platform: String,
    target: Option<String>,
    platform_package: String,
    platform_directory: PathBuf,
    application: ApplicationConfig,
}

impl BuildSelection {
    /// Returns the selected Board name.
    #[must_use]
    pub fn board(&self) -> &str {
        &self.board
    }

    /// Returns the Platform discovered from the selected Board.
    #[must_use]
    pub fn platform(&self) -> &str {
        &self.platform
    }

    /// Returns the Board's explicit Rust target, when cross-compilation is required.
    #[must_use]
    pub fn target(&self) -> Option<&str> {
        self.target.as_deref()
    }

    /// Returns the selected Platform Cargo package.
    #[must_use]
    pub fn platform_package(&self) -> &str {
        &self.platform_package
    }

    /// Returns the selected Platform directory in the generated workspace.
    #[must_use]
    pub fn platform_directory(&self) -> &Path {
        &self.platform_directory
    }

    /// Returns Platform-owned application build and launch behavior.
    #[must_use]
    pub const fn application(&self) -> &ApplicationConfig {
        &self.application
    }
}

/// Recreates a build-only workspace and injects the selected implementation aliases.
///
/// The source workspace remains static. Concrete Platform and Board HAL Cargo
/// dependencies exist only in `destination`, which is expected to live below
/// `target/`.
///
/// # Errors
///
/// Returns a diagnostic when selection, discovery, copying, or manifest
/// generation fails.
pub fn prepare_selected_workspace(
    source: &Path,
    destination: &Path,
) -> Result<BuildSelection, String> {
    let board_name = read_selected_board(source)
        .map_err(|error| error.to_string())?
        .ok_or_else(|| String::from("no Board selected; run `cargo board select` first"))?;
    let board_path = source
        .join("boards/configs")
        .join(&board_name)
        .join("board.yml");
    let board_yaml = fs::read_to_string(&board_path)
        .map_err(|error| format!("failed to read Board `{}`: {error}", board_path.display()))?;
    let board = parse(&board_yaml).map_err(|error| error.to_string())?;
    if board.name() != board_name {
        return Err(format!(
            "Board directory `{board_name}` declares Board `{}`",
            board.name()
        ));
    }
    let platform = resolve_board_platform(
        source,
        board.hardware().chip(),
        board.toolchain().map(|toolchain| toolchain.target()),
    )
    .map_err(|error| error.to_string())?;

    if destination.exists() {
        fs::remove_dir_all(destination).map_err(|error| {
            format!(
                "failed to replace generated workspace `{}`: {error}",
                destination.display()
            )
        })?;
    }
    copy_workspace(source, destination, source)?;
    let selection_source = source.join(barracuda_board_config::SELECTED_BOARD_PATH);
    let selection_destination = destination.join(barracuda_board_config::SELECTED_BOARD_PATH);
    if let Some(parent) = selection_destination.parent() {
        fs::create_dir_all(parent).map_err(|error| {
            format!(
                "failed to create generated selection directory `{}`: {error}",
                parent.display()
            )
        })?;
    }
    fs::copy(&selection_source, &selection_destination).map_err(|error| {
        format!(
            "failed to copy Board selection `{}`: {error}",
            selection_source.display()
        )
    })?;

    let platform_path = platform
        .directory()
        .strip_prefix(source)
        .map_err(|_error| String::from("selected Platform is outside the workspace"))?;
    inject_dependency(
        &destination.join("platforms/selected/Cargo.toml"),
        "barracuda-selected-platform-implementation",
        platform.package(),
        &destination.join(platform_path),
        board.platform_features(),
    )?;
    if let Some(board_hal) = board.board_hal() {
        inject_dependency(
            &destination.join("boards/selected/Cargo.toml"),
            "barracuda-selected-board-hal-implementation",
            board_hal.package(),
            &destination.join(board_hal.path()),
            &[],
        )?;
    } else if board.has_hardware_surface() {
        return Err(format!(
            "Board `{board_name}` declares hardware resources but has no `board-hal` dependency"
        ));
    }

    Ok(BuildSelection {
        board: board_name,
        platform: platform.name().to_owned(),
        target: board
            .toolchain()
            .map(|toolchain| toolchain.target().to_owned()),
        platform_package: platform.package().to_owned(),
        platform_directory: destination.join(platform_path),
        application: platform.application().clone(),
    })
}

fn copy_workspace(source: &Path, destination: &Path, root: &Path) -> Result<(), String> {
    fs::create_dir_all(destination).map_err(|error| {
        format!(
            "failed to create generated directory `{}`: {error}",
            destination.display()
        )
    })?;
    for entry in fs::read_dir(source).map_err(|error| {
        format!(
            "failed to read workspace directory `{}`: {error}",
            source.display()
        )
    })? {
        let entry = entry.map_err(|error| {
            format!(
                "failed to read workspace entry below `{}`: {error}",
                source.display()
            )
        })?;
        let name = entry.file_name();
        if source == root && matches!(name.to_str(), Some(".git" | ".barracuda" | "target")) {
            continue;
        }
        let from = entry.path();
        let to = destination.join(&name);
        let file_type = entry
            .file_type()
            .map_err(|error| format!("failed to inspect `{}`: {error}", from.display()))?;
        if file_type.is_dir() {
            copy_workspace(&from, &to, root)?;
        } else if file_type.is_file() {
            copy_file(&from, &to, &name)?;
        }
    }
    Ok(())
}

fn copy_file(source: &Path, destination: &Path, name: &OsStr) -> Result<(), String> {
    let must_copy = name == OsStr::new("Cargo.toml") || name == OsStr::new("Cargo.lock");
    let result = if must_copy {
        fs::copy(source, destination).map(|_bytes| ())
    } else {
        fs::hard_link(source, destination)
            .or_else(|_error| fs::copy(source, destination).map(|_bytes| ()))
    };
    result.map_err(|error| {
        format!(
            "failed to project `{}` into generated workspace: {error}",
            source.display()
        )
    })
}

fn inject_dependency(
    manifest: &Path,
    alias: &str,
    package: &str,
    path: &Path,
    features: &[String],
) -> Result<(), String> {
    let text = fs::read_to_string(manifest)
        .map_err(|error| format!("failed to read `{}`: {error}", manifest.display()))?;
    let mut document = text.parse::<DocumentMut>().map_err(|error| {
        format!(
            "failed to parse generated manifest `{}`: {error}",
            manifest.display()
        )
    })?;
    let mut dependency = InlineTable::new();
    dependency.insert("package", Value::from(package));
    dependency.insert("path", Value::from(path.display().to_string()));
    if !features.is_empty() {
        let mut values = Array::new();
        for feature in features {
            values.push(feature.as_str());
        }
        dependency.insert("features", Value::Array(values));
    }
    document["dependencies"][alias] = Item::Value(Value::InlineTable(dependency));
    fs::write(manifest, document.to_string())
        .map_err(|error| format!("failed to write `{}`: {error}", manifest.display()))
}
