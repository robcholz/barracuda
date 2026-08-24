//! Build-time Board YAML loading and validation.
//!
//! This crate runs on the build host. Runtime and firmware crates consume the
//! generated static description from `barracuda-board` instead of parsing YAML.

use serde::Deserialize;

/// Parsed, validated Board definition.
#[derive(Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct BoardDefinition {
    name: String,
    hardware: HardwareDefinition,
    storage: StorageDefinition,
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

    /// Returns logical storage roles bound to native-layout region names.
    #[must_use]
    pub const fn storage(&self) -> &StorageDefinition {
        &self.storage
    }

    fn validate(&self) -> Result<(), ConfigError> {
        if self.hardware.chip.trim().is_empty() {
            return Err(ConfigError::EmptyChip);
        }
        let bindings = self.storage.bindings();
        for (index, (role, region)) in bindings.iter().enumerate() {
            if region.trim().is_empty() {
                return Err(ConfigError::EmptyStorageBinding(role));
            }
            for (other_role, other_region) in bindings.iter().skip(index.saturating_add(1)) {
                if region == other_region {
                    return Err(ConfigError::DuplicateStorageBinding {
                        first: role,
                        second: other_role,
                        region: (*region).into(),
                    });
                }
            }
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

/// Logical storage consumers and their physical partition names.
#[derive(Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub struct StorageDefinition {
    filesystem: String,
    web_assets: Option<String>,
    database: String,
}

impl StorageDefinition {
    fn bindings(&self) -> Vec<(&'static str, &str)> {
        let mut bindings = vec![("filesystem", self.filesystem.as_str())];
        if let Some(web_assets) = self.web_assets.as_deref() {
            bindings.push(("web-assets", web_assets));
        }
        bindings.push(("database", self.database.as_str()));
        bindings
    }

    /// Returns the mutable filesystem partition name.
    #[must_use]
    pub fn filesystem(&self) -> &str {
        &self.filesystem
    }

    /// Returns the optional provisioned Web asset partition name.
    #[must_use]
    pub fn web_assets(&self) -> Option<&str> {
        self.web_assets.as_deref()
    }

    /// Returns the system database partition name.
    #[must_use]
    pub fn database(&self) -> &str {
        &self.database
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
    /// A Board must identify its concrete chip or host runtime.
    #[error("Board hardware chip must not be empty")]
    EmptyChip,
    /// A storage role has no native-layout label.
    #[error("storage role `{0}` must name a native-layout region")]
    EmptyStorageBinding(&'static str),
    /// Two storage roles resolve to the same native region.
    #[error("storage roles `{first}` and `{second}` both bind native region `{region}`")]
    DuplicateStorageBinding {
        /// First logical role.
        first: &'static str,
        /// Second logical role.
        second: &'static str,
        /// Duplicated native-layout region name.
        region: String,
    },
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
    let mut output = String::new();
    let assets = board
        .storage
        .web_assets()
        .map_or_else(|| "None".into(), |name| format!("Some({name:?})"));
    output.push_str(&format!(
        "/// Board selected by the build configuration.\n\
         pub const BOARD: ::barracuda_board::Board = ::barracuda_board::Board::new(\n    {:?},\n    ::barracuda_board::Hardware::new({:?}),\n    ::barracuda_board::Storage::new({:?}, {assets}, {:?}),\n);\n",
        board.name(),
        board.hardware().chip(),
        board.storage().filesystem(),
        board.storage().database(),
    ));
    output
}
