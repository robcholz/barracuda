//! Canonical parsing for Barracuda `plugin.toml` declarations.

use std::sync::OnceLock;

use regex::Regex;
use serde::Deserialize;

/// Maximum number of Unicode scalar values in a Plugin description.
pub const MAX_DESCRIPTION_CHARS: usize = 80;

/// Regular expression matched by every Plugin identity.
pub const PLUGIN_ID_PATTERN: &str = "^[a-z0-9]+(?:-[a-z0-9]+)*$";

/// One parsed Plugin declaration.
#[derive(Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct PluginManifest {
    id: String,
    #[serde(rename = "depends-on")]
    dependencies: Vec<String>,
    description: String,
}

impl PluginManifest {
    /// Returns the stable Plugin identity.
    #[must_use]
    pub fn id(&self) -> &str {
        &self.id
    }

    /// Returns the direct Plugin dependencies.
    #[must_use]
    pub fn dependencies(&self) -> &[String] {
        &self.dependencies
    }

    /// Returns the concise selection description.
    #[must_use]
    pub fn description(&self) -> &str {
        &self.description
    }
}

/// Failure while parsing a Plugin declaration.
#[derive(Debug, thiserror::Error)]
pub enum ManifestError {
    /// TOML syntax or schema validation failed.
    #[error("invalid Plugin manifest: {0}")]
    Toml(#[from] toml::de::Error),
    /// The Plugin identity is empty.
    #[error("Plugin identity must not be empty")]
    EmptyId,
    /// The Plugin identity contains leading or trailing whitespace.
    #[error("Plugin identity `{0}` must not contain leading or trailing whitespace")]
    NonCanonicalId(String),
    /// The Plugin identity does not match [`PLUGIN_ID_PATTERN`].
    #[error("Plugin identity `{0}` must match {PLUGIN_ID_PATTERN}")]
    InvalidId(String),
    /// One dependency identity is empty.
    #[error("Plugin dependency at index {0} must not be empty")]
    EmptyDependency(usize),
    /// One dependency identity contains leading or trailing whitespace.
    #[error("Plugin dependency `{0}` must not contain leading or trailing whitespace")]
    NonCanonicalDependency(String),
    /// One dependency identity does not match [`PLUGIN_ID_PATTERN`].
    #[error("Plugin dependency `{0}` must match {PLUGIN_ID_PATTERN}")]
    InvalidDependency(String),
    /// The same direct dependency is declared more than once.
    #[error("Plugin dependency `{0}` is declared more than once")]
    DuplicateDependency(String),
    /// A Plugin declares itself as a dependency.
    #[error("Plugin `{0}` cannot depend on itself")]
    SelfDependency(String),
    /// The Plugin description is empty after trimming whitespace.
    #[error("Plugin description must not be empty")]
    EmptyDescription,
    /// The Plugin description exceeds [`MAX_DESCRIPTION_CHARS`].
    #[error("Plugin description is {length} characters; maximum is {MAX_DESCRIPTION_CHARS}")]
    DescriptionTooLong {
        /// Actual number of Unicode scalar values.
        length: usize,
    },
}

/// Parses one `plugin.toml` declaration.
///
/// # Errors
///
/// Returns an error when the TOML does not match the Plugin manifest schema.
pub fn parse(contents: &str) -> Result<PluginManifest, ManifestError> {
    let mut manifest = toml::from_str::<PluginManifest>(contents)?;
    validate_identity(&manifest.id)?;
    for (index, dependency) in manifest.dependencies.iter().enumerate() {
        if dependency.trim().is_empty() {
            return Err(ManifestError::EmptyDependency(index));
        }
        if dependency.trim() != dependency {
            return Err(ManifestError::NonCanonicalDependency(dependency.clone()));
        }
        if !matches_plugin_id_pattern(dependency) {
            return Err(ManifestError::InvalidDependency(dependency.clone()));
        }
        if manifest.dependencies[..index].contains(dependency) {
            return Err(ManifestError::DuplicateDependency(dependency.clone()));
        }
        if dependency == &manifest.id {
            return Err(ManifestError::SelfDependency(manifest.id.clone()));
        }
    }
    let description = manifest.description.trim();
    if description.is_empty() {
        return Err(ManifestError::EmptyDescription);
    }
    let length = description.chars().count();
    if length > MAX_DESCRIPTION_CHARS {
        return Err(ManifestError::DescriptionTooLong { length });
    }
    manifest.description = description.to_owned();
    Ok(manifest)
}

fn validate_identity(identity: &str) -> Result<(), ManifestError> {
    if identity.trim().is_empty() {
        return Err(ManifestError::EmptyId);
    }
    if identity.trim() != identity {
        return Err(ManifestError::NonCanonicalId(identity.to_owned()));
    }
    if !matches_plugin_id_pattern(identity) {
        return Err(ManifestError::InvalidId(identity.to_owned()));
    }
    Ok(())
}

fn matches_plugin_id_pattern(identity: &str) -> bool {
    static REGEX: OnceLock<Result<Regex, regex::Error>> = OnceLock::new();

    REGEX
        .get_or_init(|| Regex::new(PLUGIN_ID_PATTERN))
        .as_ref()
        .is_ok_and(|regex| regex.is_match(identity))
}
