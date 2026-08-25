//! Build-time Board YAML loading and validation.
//!
//! This crate runs on the build host. Runtime and firmware crates consume the
//! generated static description from `barracuda-board` instead of parsing YAML.

use serde::Deserialize;
use std::{
    fs, io,
    path::{Component, Path, PathBuf},
};

/// Workspace-relative file containing the persistently selected Board name.
pub const SELECTED_BOARD_PATH: &str = ".barracuda/selected-board";

/// Failure while reading or writing the persistent Board selection.
#[derive(Debug, thiserror::Error)]
pub enum SelectionError {
    /// The persisted or requested Board name is unsafe or malformed.
    #[error("invalid Board name `{name}`")]
    InvalidName {
        /// Invalid value.
        name: String,
    },
    /// Local selection state could not be read or written.
    #[error("failed to access Board selection at `{path}`: {source}")]
    Io {
        /// State path that failed.
        path: PathBuf,
        /// Underlying filesystem failure.
        #[source]
        source: io::Error,
    },
}

/// Reads the Board selected for subsequent workspace builds.
///
/// # Errors
///
/// Returns [`SelectionError`] when the state file cannot be read or contains
/// an invalid Board name. A missing state file means no Board has been selected.
pub fn read_selected_board(workspace_root: &Path) -> Result<Option<String>, SelectionError> {
    let path = workspace_root.join(SELECTED_BOARD_PATH);
    let selected = match fs::read_to_string(&path) {
        Ok(selected) => selected,
        Err(source) if source.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(source) => return Err(SelectionError::Io { path, source }),
    };
    let name = selected.trim_end_matches(['\r', '\n']);
    validate_board_name(name)?;
    Ok(Some(name.to_owned()))
}

/// Persists the Board used by subsequent workspace builds.
///
/// # Errors
///
/// Returns [`SelectionError`] when `name` is invalid or local state cannot be
/// created and written.
pub fn write_selected_board(workspace_root: &Path, name: &str) -> Result<(), SelectionError> {
    validate_board_name(name)?;
    let path = workspace_root.join(SELECTED_BOARD_PATH);
    let parent = path.parent().unwrap_or(workspace_root);
    fs::create_dir_all(parent).map_err(|source| SelectionError::Io {
        path: parent.to_owned(),
        source,
    })?;
    fs::write(&path, format!("{name}\n")).map_err(|source| SelectionError::Io { path, source })
}

/// Validates a Board name before it is used as a bundle-directory component.
///
/// # Errors
///
/// Returns [`SelectionError::InvalidName`] for empty names or values containing
/// characters other than ASCII letters, digits, `-`, and `_`.
pub fn validate_board_name(name: &str) -> Result<(), SelectionError> {
    if !name.is_empty()
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        Ok(())
    } else {
        Err(SelectionError::InvalidName {
            name: name.to_owned(),
        })
    }
}

/// Parsed, validated Board definition.
#[derive(Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct BoardDefinition {
    name: String,
    hardware: HardwareDefinition,
    #[serde(default)]
    toolchain: Option<ToolchainDefinition>,
    #[serde(rename = "native-layout")]
    native_layout: NativeLayoutDefinition,
}

impl BoardDefinition {
    /// Returns the stable Board name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Returns the concrete hardware identity.
    #[must_use]
    pub const fn hardware(&self) -> &HardwareDefinition {
        &self.hardware
    }

    /// Returns the cross-compilation toolchain declaration, when the Board has one.
    ///
    /// A Board that must be cross-compiled declares the Rust target triple it is
    /// built for. Host Boards omit this and build for the build host's native
    /// target. The target is build policy, not a runtime hardware fact, so it is
    /// consumed by the build driver (for example CI) and never baked into the
    /// generated runtime Board value.
    #[must_use]
    pub const fn toolchain(&self) -> Option<&ToolchainDefinition> {
        self.toolchain.as_ref()
    }

    /// Returns the Board-bundled native physical layout.
    #[must_use]
    pub const fn native_layout(&self) -> &NativeLayoutDefinition {
        &self.native_layout
    }

    fn validate(&self) -> Result<(), ConfigError> {
        if self.hardware.chip.trim().is_empty() {
            return Err(ConfigError::EmptyChip);
        }
        if let Some(toolchain) = &self.toolchain {
            if toolchain.target.trim().is_empty() {
                return Err(ConfigError::EmptyToolchainTarget);
            }
        }
        let artifact = self.native_layout.artifact.trim();
        if artifact.is_empty() {
            return Err(ConfigError::EmptyNativeLayoutArtifact);
        }
        let path = Path::new(artifact);
        if path.is_absolute()
            || path
                .components()
                .any(|component| !matches!(component, Component::Normal(_)))
        {
            return Err(ConfigError::InvalidNativeLayoutArtifact);
        }
        Ok(())
    }
}

/// Concrete hardware facts shared with Platform build validation.
#[derive(Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct HardwareDefinition {
    chip: String,
}

impl HardwareDefinition {
    /// Returns the canonical HAL chip name.
    #[must_use]
    pub fn chip(&self) -> &str {
        &self.chip
    }
}

/// Toolchain policy a Board carries so the build driver can choose a target.
#[derive(Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ToolchainDefinition {
    target: String,
}

impl ToolchainDefinition {
    /// Returns the Rust target triple this Board is cross-compiled for.
    #[must_use]
    pub fn target(&self) -> &str {
        &self.target
    }
}

/// A Platform-native layout file stored inside the Board bundle.
#[derive(Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct NativeLayoutDefinition {
    artifact: String,
}

impl NativeLayoutDefinition {
    /// Returns the artifact path relative to the Board bundle.
    #[must_use]
    pub fn artifact(&self) -> &str {
        &self.artifact
    }
}

/// Failure while parsing or validating Board YAML.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ConfigError {
    /// YAML syntax or shape is invalid.
    #[error("invalid Board YAML: {0}")]
    Yaml(String),
    /// Exactly one YAML document is required.
    #[error("expected one Board YAML document, found {0}")]
    DocumentCount(usize),
    /// A Board must identify its concrete chip or native operating-system runtime.
    #[error("Board hardware chip must not be empty")]
    EmptyChip,
    /// A Board declared a toolchain section without a target triple.
    #[error("Board toolchain target must not be empty")]
    EmptyToolchainTarget,
    /// A Board did not identify its native physical-layout artifact.
    #[error("Board native-layout artifact must not be empty")]
    EmptyNativeLayoutArtifact,
    /// A native-layout artifact is absolute or escapes the Board bundle.
    #[error("Board native-layout artifact must remain inside its Board bundle")]
    InvalidNativeLayoutArtifact,
}

/// Parses and validates exactly one platform-neutral Board YAML document.
///
/// # Errors
///
/// Returns [`ConfigError`] for malformed YAML or ambiguous native-layout bindings.
pub fn parse(yaml: &str) -> Result<BoardDefinition, ConfigError> {
    let mut documents = yaml_peg::serde::from_str::<BoardDefinition>(yaml)
        .map_err(|error| ConfigError::Yaml(error.to_string()))?;
    if documents.len() != 1 {
        return Err(ConfigError::DocumentCount(documents.len()));
    }
    let board = documents.pop().ok_or(ConfigError::DocumentCount(0))?;
    board.validate()?;
    Ok(board)
}

/// Renders a validated Board definition as a static `barracuda-board` value.
///
/// The output contains no Platform type or implementation choice. A build
/// entry independently selects a Platform and passes this generated value to
/// that Platform's initializer.
#[must_use]
pub fn render_rust(board: &BoardDefinition) -> String {
    format!(
        "/// Board selected by the build configuration.\n\
         pub const BOARD: ::barracuda_board::Board = ::barracuda_board::Board::new(\n    {:?},\n    ::barracuda_board::Hardware::new({:?}),\n    ::barracuda_board::NativeLayout::new({:?}),\n);\n",
        board.name(),
        board.hardware().chip(),
        board.native_layout().artifact(),
    )
}
