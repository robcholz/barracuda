//! Agent Skills identity, discovery metadata, and `SKILL.md` parsing.

use alloc::borrow::Cow;
use alloc::collections::BTreeMap;
use alloc::format;
use alloc::string::{String, ToString};
use core::fmt;

use barracuda_vfs::FsError;
use serde::Deserialize;
use thiserror::Error;

const MAX_NAME_CHARS: usize = 64;
const MAX_DESCRIPTION_CHARS: usize = 1024;
const MAX_COMPATIBILITY_CHARS: usize = 500;

/// A skill's standard name and its directory name under a skills root.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SkillName(Cow<'static, str>);

impl SkillName {
    /// Construct a skill name. Filesystem registries validate it while scanning.
    pub fn new(name: impl Into<String>) -> Self {
        Self(Cow::Owned(name.into()))
    }

    /// Wrap a static name without allocation.
    pub const fn from_static(name: &'static str) -> Self {
        Self(Cow::Borrowed(name))
    }

    /// The name as a string slice.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for SkillName {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// Discovery metadata for one standard Agent Skill.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Skill {
    name: SkillName,
    description: String,
    license: Option<String>,
    compatibility: Option<String>,
    metadata: BTreeMap<String, String>,
    allowed_tools: Option<String>,
    directory: Option<String>,
}

impl Skill {
    /// Build the required discovery metadata supplied by an external registry.
    pub fn new(name: SkillName, description: String) -> Result<Self, SkillError> {
        validate_name(&name)?;
        validate_required_text(&name, "description", &description, MAX_DESCRIPTION_CHARS)?;
        Ok(Self {
            name,
            description,
            license: None,
            compatibility: None,
            metadata: BTreeMap::new(),
            allowed_tools: None,
            directory: None,
        })
    }

    /// The `name` declared in frontmatter and matched to the parent directory.
    pub fn name(&self) -> &SkillName {
        &self.name
    }

    /// What the skill does and when an agent should use it.
    pub fn description(&self) -> &str {
        &self.description
    }

    /// Optional license name or reference to a bundled license file.
    pub fn license(&self) -> Option<&str> {
        self.license.as_deref()
    }

    /// Optional environment requirements.
    pub fn compatibility(&self) -> Option<&str> {
        self.compatibility.as_deref()
    }

    /// Optional implementation-specific string metadata.
    pub fn metadata(&self) -> &BTreeMap<String, String> {
        &self.metadata
    }

    /// Optional experimental space-separated pre-approved tool declaration.
    pub fn allowed_tools(&self) -> Option<&str> {
        self.allowed_tools.as_deref()
    }

    /// Filesystem directory used to resolve relative skill resources.
    ///
    /// External registries may omit this when they resolve resources through a
    /// non-filesystem backend.
    pub fn directory(&self) -> Option<&str> {
        self.directory.as_deref()
    }
}

/// An owned snapshot of a skill's Markdown instructions.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SkillDocument {
    content: String,
    directory: Option<String>,
}

impl SkillDocument {
    pub(crate) fn new(content: String, directory: Option<String>) -> Self {
        Self { content, directory }
    }

    /// Markdown below the `SKILL.md` frontmatter.
    pub fn content(&self) -> &str {
        &self.content
    }

    /// Consume the snapshot and return its Markdown instructions.
    pub fn into_content(self) -> String {
        self.content
    }

    /// Filesystem directory used to resolve relative resource paths.
    pub fn directory(&self) -> Option<&str> {
        self.directory.as_deref()
    }

    /// Consume the snapshot and return its instructions and optional directory.
    pub fn into_parts(self) -> (String, Option<String>) {
        (self.content, self.directory)
    }
}

/// Failure reading, parsing, or resolving a standard Agent Skill.
#[derive(Clone, Debug, PartialEq, Eq, Error)]
pub enum SkillError {
    /// No skill with the given name is registered.
    #[error("skill not found: {0}")]
    NotFound(SkillName),
    /// Listing a skills root directory failed.
    #[error("failed to scan skills root '{0}': {1}")]
    ScanFailed(String, FsError),
    /// Reading a skill's `SKILL.md` failed.
    #[error("failed to read skill '{0}': {1}")]
    ReadFailed(SkillName, FsError),
    /// A skill's `SKILL.md` bytes were not valid UTF-8.
    #[error("skill '{0}' is not valid UTF-8")]
    InvalidUtf8(SkillName),
    /// A skill's frontmatter is missing its opening `---` line.
    #[error("skill '{0}' is missing the opening '---' frontmatter fence")]
    MissingOpeningFence(SkillName),
    /// A skill's frontmatter is missing its closing `---` line.
    #[error("skill '{0}' is missing the closing '---' frontmatter fence")]
    MissingClosingFence(SkillName),
    /// A skill's frontmatter is not valid YAML.
    #[error("skill '{0}' has invalid YAML frontmatter: {1}")]
    InvalidYaml(SkillName, String),
    /// Parsed frontmatter violates the Agent Skills specification.
    #[error("skill '{0}' has invalid frontmatter: {1}")]
    InvalidFrontmatter(SkillName, String),
    /// A non-filesystem registry operation failed.
    #[error("skill backend operation '{operation}' failed with code {code}")]
    Backend {
        /// Stable operation label supplied by the backend.
        operation: &'static str,
        /// Backend-native error code.
        code: i32,
    },
}

#[derive(Deserialize)]
#[serde(rename_all = "kebab-case")]
struct RawFrontmatter {
    name: Option<String>,
    description: Option<String>,
    license: Option<String>,
    compatibility: Option<String>,
    #[serde(default)]
    metadata: BTreeMap<String, String>,
    allowed_tools: Option<String>,
}

pub(crate) fn parse_frontmatter(
    directory_name: SkillName,
    root: &str,
    document: &str,
) -> Result<Skill, SkillError> {
    validate_name(&directory_name)?;
    let (yaml, _) = frontmatter_sections(&directory_name, document)?;
    if yaml.trim_start().starts_with('{') {
        return Err(SkillError::InvalidYaml(
            directory_name.clone(),
            "JSON object frontmatter is not accepted; use standard YAML mappings".into(),
        ));
    }
    let mut documents = yaml_peg::serde::from_str::<RawFrontmatter>(yaml)
        .map_err(|error| SkillError::InvalidYaml(directory_name.clone(), error.to_string()))?;
    if documents.len() != 1 {
        return Err(SkillError::InvalidYaml(
            directory_name.clone(),
            "frontmatter must contain exactly one YAML document".into(),
        ));
    }
    let raw = documents.remove(0);

    let name = required_string(&directory_name, "name", raw.name)?;
    if name != directory_name.as_str() {
        return Err(SkillError::InvalidFrontmatter(
            directory_name,
            format!("frontmatter name '{name}' must match the skill directory name"),
        ));
    }
    let description = required_string(&directory_name, "description", raw.description)?;
    validate_required_text(
        &directory_name,
        "description",
        &description,
        MAX_DESCRIPTION_CHARS,
    )?;
    validate_optional_text(
        &directory_name,
        "compatibility",
        raw.compatibility.as_deref(),
        MAX_COMPATIBILITY_CHARS,
    )?;

    Ok(Skill {
        name: directory_name,
        description,
        license: raw.license,
        compatibility: raw.compatibility,
        metadata: raw.metadata,
        allowed_tools: raw.allowed_tools,
        directory: Some(format!("{}/{}", root.trim_end_matches('/'), name)),
    })
}

pub(crate) fn frontmatter_sections<'a>(
    name: &SkillName,
    text: &'a str,
) -> Result<(&'a str, &'a str), SkillError> {
    let after_open = text
        .strip_prefix("---\n")
        .or_else(|| text.strip_prefix("---\r\n"))
        .ok_or_else(|| SkillError::MissingOpeningFence(name.clone()))?;

    let mut offset = 0;
    for line_with_ending in after_open.split_inclusive('\n') {
        let line = line_with_ending.trim_end_matches(['\r', '\n']);
        if line == "---" {
            let body_start = offset + line_with_ending.len();
            return Ok((&after_open[..offset], &after_open[body_start..]));
        }
        offset += line_with_ending.len();
    }
    if after_open[offset..]
        .strip_suffix('\r')
        .unwrap_or(&after_open[offset..])
        == "---"
    {
        return Ok((&after_open[..offset], ""));
    }
    Err(SkillError::MissingClosingFence(name.clone()))
}

fn validate_name(name: &SkillName) -> Result<(), SkillError> {
    let value = name.as_str();
    let len = value.chars().count();
    let valid = (1..=MAX_NAME_CHARS).contains(&len)
        && !value.starts_with('-')
        && !value.ends_with('-')
        && !value.contains("--")
        && value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-');
    if valid {
        Ok(())
    } else {
        Err(SkillError::InvalidFrontmatter(
            name.clone(),
            "name must be 1-64 lowercase ASCII letters, digits, or hyphens; it cannot start or end with a hyphen or contain consecutive hyphens".into(),
        ))
    }
}

fn required_string(
    name: &SkillName,
    field: &'static str,
    value: Option<String>,
) -> Result<String, SkillError> {
    value.ok_or_else(|| SkillError::InvalidFrontmatter(name.clone(), format!("missing {field}")))
}

fn validate_required_text(
    name: &SkillName,
    field: &'static str,
    value: &str,
    max_chars: usize,
) -> Result<(), SkillError> {
    if value.trim().is_empty() || value.chars().count() > max_chars {
        return Err(SkillError::InvalidFrontmatter(
            name.clone(),
            format!("{field} must contain 1-{max_chars} characters"),
        ));
    }
    Ok(())
}

fn validate_optional_text(
    name: &SkillName,
    field: &'static str,
    value: Option<&str>,
    max_chars: usize,
) -> Result<(), SkillError> {
    if let Some(value) = value {
        if value.trim().is_empty() || value.chars().count() > max_chars {
            return Err(SkillError::InvalidFrontmatter(
                name.clone(),
                format!("{field} must contain 1-{max_chars} characters when present"),
            ));
        }
    }
    Ok(())
}
