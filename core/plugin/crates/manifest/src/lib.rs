//! Canonical parsing for Barracuda `plugin.toml` declarations.

use std::{
    collections::BTreeMap,
    path::{Component, Path},
    sync::OnceLock,
};

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
    /// System-owned resources moved into the Plugin constructor.
    #[serde(default, rename = "system-resources")]
    system_resources: Vec<String>,
    /// Author-declared host tasks, keyed by task name.
    #[serde(default)]
    pub tasks: BTreeMap<String, PluginTask>,
}

/// A host command; paths are relative to the Plugin root, not to `cwd`.
#[derive(Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct PluginTask {
    /// Working directory within the Plugin.
    #[serde(default = "default_cwd")]
    pub cwd: String,
    /// Executable followed by literal arguments; no implicit shell.
    pub command: Vec<String>,
    /// Files or directories watched by Cargo. Explicit shared inputs may use `..`.
    #[serde(default)]
    pub inputs: Vec<String>,
    /// Expected output files or directories within the Plugin.
    #[serde(default)]
    pub outputs: Vec<String>,
    /// Environment variables that affect the task's outputs.
    #[serde(default)]
    pub env: Vec<String>,
}

fn default_cwd() -> String {
    String::from(".")
}

fn local_path(value: &str) -> bool {
    !value.is_empty()
        && !value.contains(['\n', '\r', '\0', '\\'])
        && Path::new(value)
            .components()
            .all(|part| matches!(part, Component::Normal(_) | Component::CurDir))
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

    /// Returns System resource field names consumed by the Plugin constructor.
    #[must_use]
    pub fn system_resources(&self) -> &[String] {
        &self.system_resources
    }
}

/// Failure while parsing a Plugin declaration.
#[derive(Debug, thiserror::Error)]
pub enum ManifestError {
    /// A task contains an invalid name, command or path.
    #[error(
        "invalid Plugin task `{0}`: expected a command, relative paths and valid environment names"
    )]
    InvalidTask(String),
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
    /// A System resource name is not a Rust field identifier.
    #[error("Plugin System resource `{0}` must be a lowercase Rust field identifier")]
    InvalidSystemResource(String),
    /// The same System resource is declared more than once.
    #[error("Plugin System resource `{0}` is declared more than once")]
    DuplicateSystemResource(String),
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
    for (name, task) in &manifest.tasks {
        if !matches_plugin_id_pattern(name)
            || task
                .command
                .first()
                .is_none_or(|program| program.trim().is_empty())
            || task.command.iter().any(|arg| arg.contains('\0'))
            || !local_path(&task.cwd)
            || task.outputs.iter().any(|path| !local_path(path))
            || task.inputs.iter().any(|path| {
                path.is_empty()
                    || Path::new(path).is_absolute()
                    || path.contains(['\n', '\r', '\0'])
            })
            || task
                .env
                .iter()
                .any(|name| name.is_empty() || name.contains(['=', '\0', '\n', '\r']))
        {
            return Err(ManifestError::InvalidTask(name.clone()));
        }
    }
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
    for (index, resource) in manifest.system_resources.iter().enumerate() {
        let mut characters = resource.chars();
        let valid = characters
            .next()
            .is_some_and(|character| character.is_ascii_lowercase())
            && characters.all(|character| {
                character.is_ascii_lowercase() || character.is_ascii_digit() || character == '_'
            });
        if !valid {
            return Err(ManifestError::InvalidSystemResource(resource.clone()));
        }
        if manifest.system_resources[..index].contains(resource) {
            return Err(ManifestError::DuplicateSystemResource(resource.clone()));
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
