//! Build-time Board YAML loading and validation.
//!
//! This crate runs on the build host. Runtime and firmware crates consume the
//! generated static description from `barracuda-board` instead of parsing YAML.

use serde::Deserialize;
use std::path::{Component, Path};

/// Parsed, validated Board definition.
#[derive(Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct BoardDefinition {
    name: String,
    hardware: HardwareDefinition,
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

    /// Returns the Board-bundled native physical layout.
    #[must_use]
    pub const fn native_layout(&self) -> &NativeLayoutDefinition {
        &self.native_layout
    }

    fn validate(&self) -> Result<(), ConfigError> {
        if self.hardware.chip.trim().is_empty() {
            return Err(ConfigError::EmptyChip);
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
