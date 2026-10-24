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
    /// Author-declared host tasks, keyed by task name.
    #[serde(default)]
    pub tasks: BTreeMap<String, PluginTask>,
    /// Settings a Board may override in its `plugins.<id>` section, keyed by
    /// setting name.
    #[serde(default)]
    pub config: BTreeMap<String, ConfigSetting>,
    /// Peripheral slots the Plugin claims, keyed by peripheral kind and then
    /// by the Plugin's own slot name.
    #[serde(default)]
    pub peripherals: BTreeMap<String, BTreeMap<String, PeripheralClaim>>,
    /// Platform mechanisms the Plugin takes from its context.
    #[serde(default)]
    pub platform: PlatformClaims,
}

/// Platform mechanisms a Plugin takes from `PluginContext`, declared under
/// `[platform]`. A firmware builds a mechanism only when an enabled Plugin
/// takes it.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct PlatformClaims {
    /// Whether the Plugin takes the Platform's Wi-Fi radio,
    /// `PluginContext::wifi`.
    #[serde(default)]
    pub wifi: bool,
}

/// What fills one peripheral slot.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PeripheralFill {
    /// The device the Board marks primary for the slot's kind.
    Primary,
    /// The only device of the slot's kind that nothing else takes.
    Any,
    /// Every device of the slot's kind that nothing else takes.
    Rest,
}

impl PeripheralFill {
    /// Returns the `plugin.toml` spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Primary => "primary",
            Self::Any => "any",
            Self::Rest => "*",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        match value {
            "primary" => Some(Self::Primary),
            "any" => Some(Self::Any),
            "*" => Some(Self::Rest),
            _ => None,
        }
    }
}

/// One peripheral slot a Plugin claims.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PeripheralClaim {
    /// What fills the slot.
    pub fill: PeripheralFill,
    /// Whether the Plugin starts without the slot filled. A `"*"` slot is
    /// always optional: it may receive no device.
    pub optional: bool,
}

impl<'de> Deserialize<'de> for PeripheralClaim {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Detailed {
            fill: String,
            #[serde(default)]
            optional: bool,
        }
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Raw {
            Fill(String),
            Detailed(Detailed),
        }
        let (fill, optional) = match Raw::deserialize(deserializer)? {
            Raw::Fill(fill) => (fill, false),
            Raw::Detailed(Detailed { fill, optional }) => (fill, optional),
        };
        let fill = PeripheralFill::parse(&fill).ok_or_else(|| {
            serde::de::Error::custom(format!(
                "peripheral slot fill `{fill}` must be \"primary\", \"any\" or \"*\""
            ))
        })?;
        Ok(Self {
            fill,
            optional: optional || fill == PeripheralFill::Rest,
        })
    }
}

/// One Board-configurable Plugin setting, generated into a constant.
#[derive(Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ConfigSetting {
    /// Rust type of the generated constant.
    #[serde(rename = "type")]
    pub kind: ConfigType,
    /// Value used when the selected Board does not set one.
    pub default: ConfigValue,
    /// What the setting controls, emitted as the constant's documentation.
    pub description: String,
}

/// Rust type of a generated Plugin setting.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum ConfigType {
    /// `bool`.
    Bool,
    /// `u8`.
    U8,
    /// `u16`.
    U16,
    /// `u32`.
    U32,
    /// `usize`.
    Usize,
    /// `i32`.
    I32,
    /// `&'static str`.
    Str,
}

impl ConfigType {
    /// Returns the Rust type of the generated constant.
    #[must_use]
    pub const fn rust_type(self) -> &'static str {
        match self {
            Self::Bool => "bool",
            Self::U8 => "u8",
            Self::U16 => "u16",
            Self::U32 => "u32",
            Self::Usize => "usize",
            Self::I32 => "i32",
            Self::Str => "&str",
        }
    }

    /// Returns whether `value` is a valid value of this type.
    #[must_use]
    pub fn accepts(self, value: &ConfigValue) -> bool {
        match (self, value) {
            (Self::Bool, ConfigValue::Bool(_)) | (Self::Str, ConfigValue::String(_)) => true,
            (Self::U8, ConfigValue::Integer(value)) => u8::try_from(*value).is_ok(),
            (Self::U16, ConfigValue::Integer(value)) => u16::try_from(*value).is_ok(),
            (Self::U32, ConfigValue::Integer(value)) => u32::try_from(*value).is_ok(),
            (Self::Usize, ConfigValue::Integer(value)) => u32::try_from(*value).is_ok(),
            (Self::I32, ConfigValue::Integer(value)) => i32::try_from(*value).is_ok(),
            _ => false,
        }
    }
}

/// A Plugin setting value, as written in `plugin.toml` or a Board.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(untagged)]
pub enum ConfigValue {
    /// Boolean value.
    Bool(bool),
    /// Integer value.
    Integer(i64),
    /// Text value.
    String(String),
}

impl ConfigValue {
    /// Returns the value as a Rust literal.
    #[must_use]
    pub fn rust_literal(&self) -> String {
        match self {
            Self::Bool(value) => value.to_string(),
            Self::Integer(value) => value.to_string(),
            Self::String(value) => format!("{value:?}"),
        }
    }
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
    /// A Plugin declares itself as a dependency.
    #[error("Plugin `{0}` cannot depend on itself")]
    SelfDependency(String),
    /// The Plugin description is empty after trimming whitespace.
    #[error("Plugin description must not be empty")]
    EmptyDescription,
    /// A setting has an invalid name, an empty description, or a default
    /// that does not fit its type.
    #[error(
        "invalid Plugin setting `{0}`: expected a lowercase name, a description and a default of its type"
    )]
    InvalidSetting(String),
    /// A peripheral kind or slot name is not a lowercase name.
    #[error("invalid peripheral slot `{0}`: expected lowercase kind and slot names")]
    InvalidPeripheralSlot(String),
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
    for (name, setting) in &manifest.config {
        if !matches_plugin_id_pattern(name)
            || setting.description.trim().is_empty()
            || !setting.kind.accepts(&setting.default)
        {
            return Err(ManifestError::InvalidSetting(name.clone()));
        }
    }
    for (kind, slots) in &manifest.peripherals {
        for slot in slots.keys() {
            if !matches_plugin_id_pattern(kind) || !matches_plugin_id_pattern(slot) {
                return Err(ManifestError::InvalidPeripheralSlot(format!(
                    "{kind}.{slot}"
                )));
            }
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
