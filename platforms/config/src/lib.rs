//! Filesystem discovery and host-side resolution of Barracuda Platforms.

use std::collections::BTreeMap;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

use serde::Deserialize;

/// Cargo target properties used to select a concrete Platform.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PlatformTarget<'a> {
    triple: &'a str,
    os: &'a str,
    arch: &'a str,
}

impl<'a> PlatformTarget<'a> {
    /// Creates a target description from Cargo's `TARGET` and `CARGO_CFG_*` values.
    #[must_use]
    pub const fn new(triple: &'a str, os: &'a str, arch: &'a str) -> Self {
        Self { triple, os, arch }
    }
}

/// A command-backed Platform operation.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CommandDriver {
    program: PathBuf,
    #[serde(default)]
    arguments: Vec<String>,
}

impl CommandDriver {
    /// Returns the workspace-relative executable path or command name.
    #[must_use]
    pub fn program(&self) -> &Path {
        &self.program
    }

    /// Returns the argument templates passed without a shell.
    #[must_use]
    pub fn arguments(&self) -> &[String] {
        &self.arguments
    }
}

/// Platform-owned System image layout resolution.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(tag = "driver", rename_all = "kebab-case")]
pub enum LayoutDriver {
    /// Barracuda host file-region YAML.
    FileRegions,
    /// ESP-IDF partition CSV.
    EspIdfPartitions,
    /// Linker `MEMORY` region declarations.
    LinkerMemory,
    /// Platform-local command returning the standard layout response.
    Command(CommandDriver),
}

/// Platform-owned System image flashing operation.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(tag = "driver", rename_all = "kebab-case")]
pub enum FlashDriver {
    /// Host file-backed flash configured in the Platform manifest.
    File {
        /// Workspace-relative directory containing the emulated flash.
        #[serde(rename = "state-directory")]
        state_directory: PathBuf,
        /// Path below `state-directory` containing the emulated flash bytes.
        #[serde(rename = "flash-image")]
        flash_image: PathBuf,
    },
    /// Platform-local command using standard argument templates.
    Command(CommandDriver),
}

/// System image behavior declared by one Platform.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SystemImageConfig {
    layout: LayoutDriver,
    flash: FlashDriver,
}

/// Platform-owned support binaries and optional application launcher.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ApplicationConfig {
    #[serde(default, rename = "support-binaries")]
    support_binaries: Vec<String>,
    #[serde(default)]
    launcher: Option<CommandDriver>,
}

impl ApplicationConfig {
    /// Returns Platform package binaries built alongside the application.
    #[must_use]
    pub fn support_binaries(&self) -> &[String] {
        &self.support_binaries
    }

    /// Returns the optional command used to launch the built application.
    #[must_use]
    pub const fn launcher(&self) -> Option<&CommandDriver> {
        self.launcher.as_ref()
    }
}

impl SystemImageConfig {
    /// Returns the Platform's native System layout resolver.
    #[must_use]
    pub const fn layout(&self) -> &LayoutDriver {
        &self.layout
    }

    /// Returns the Platform's System partition flasher.
    #[must_use]
    pub const fn flash(&self) -> &FlashDriver {
        &self.flash
    }
}

impl FlashDriver {
    /// Returns file-backed flash paths when this is the built-in file driver.
    #[must_use]
    pub fn file_paths(&self) -> Option<(&Path, &Path)> {
        match self {
            Self::File {
                state_directory,
                flash_image,
            } => Some((state_directory, flash_image)),
            Self::Command(_) => None,
        }
    }
}

/// One self-described Platform discovered below `platforms/`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlatformDefinition {
    name: String,
    package: String,
    crate_name: String,
    type_name: String,
    directory: PathBuf,
    selection: SelectionConfig,
    system_image: SystemImageConfig,
    application: ApplicationConfig,
}

impl PlatformDefinition {
    /// Returns the stable Platform name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Returns the Platform Cargo package name.
    #[must_use]
    pub fn package(&self) -> &str {
        &self.package
    }

    /// Returns the Platform Rust crate identifier.
    #[must_use]
    pub fn crate_name(&self) -> &str {
        &self.crate_name
    }

    /// Returns the concrete Platform type exported by its crate.
    #[must_use]
    pub fn type_name(&self) -> &str {
        &self.type_name
    }

    /// Returns the Platform bundle directory.
    #[must_use]
    pub fn directory(&self) -> &Path {
        &self.directory
    }

    /// Returns the Platform-owned System image behavior.
    #[must_use]
    pub const fn system_image(&self) -> &SystemImageConfig {
        &self.system_image
    }

    /// Returns Platform-owned application build and launch behavior.
    #[must_use]
    pub const fn application(&self) -> &ApplicationConfig {
        &self.application
    }

    /// Returns Platform-owned Cargo features needed by one supported chip.
    #[must_use]
    pub fn cargo_features_for_chip(&self, chip: &str) -> &[String] {
        self.selection
            .features_by_chip
            .get(chip)
            .map_or(&[], Vec::as_slice)
    }

    /// Returns Platform-owned compiler flags for one concrete Cargo target.
    #[must_use]
    pub fn cargo_rustflags_for_target(&self, target: &str) -> &[String] {
        self.selection
            .targets
            .iter()
            .find(|selector| {
                selector
                    .triple
                    .as_deref()
                    .is_some_and(|pattern| wildcard_matches(pattern, target))
            })
            .map_or(&[], |selector| selector.rustflags.as_slice())
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
struct SelectionConfig {
    board_chips: Vec<String>,
    targets: Vec<TargetSelector>,
    #[serde(default)]
    features_by_chip: BTreeMap<String, Vec<String>>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct TargetSelector {
    #[serde(default)]
    triple: Option<String>,
    #[serde(default)]
    os: Option<String>,
    #[serde(default)]
    arch: Option<String>,
    #[serde(default)]
    rustflags: Vec<String>,
}

#[derive(Deserialize)]
struct PlatformDocument {
    name: String,
    package: String,
    #[serde(rename = "crate")]
    crate_name: String,
    #[serde(rename = "type")]
    type_name: String,
    selection: SelectionConfig,
    #[serde(rename = "system-image")]
    system_image: SystemImageConfig,
    #[serde(default)]
    application: ApplicationConfig,
}

/// Failure while discovering or resolving a Platform.
#[derive(Debug)]
#[non_exhaustive]
pub enum ResolveError {
    /// The Platform catalog cannot be read.
    Catalog {
        /// Catalog directory that could not be read.
        path: PathBuf,
        /// Underlying filesystem failure.
        source: std::io::Error,
    },
    /// A Platform manifest cannot be read.
    ManifestRead {
        /// Manifest path that could not be read.
        path: PathBuf,
        /// Underlying filesystem failure.
        source: std::io::Error,
    },
    /// A Platform manifest is malformed.
    ManifestInvalid {
        /// Invalid manifest path.
        path: PathBuf,
        /// Validation or parser diagnostic.
        message: String,
    },
    /// The Platform directory and manifest names differ.
    NameMismatch {
        /// Platform bundle directory name.
        directory: String,
        /// Name declared inside the manifest.
        declared: String,
    },
    /// No Platform matches the requested Board or target.
    NoMatch(String),
    /// More than one Platform matches the requested Board or target.
    Ambiguous {
        /// Selection request that matched multiple Platforms.
        request: String,
        /// Matching Platform names.
        platforms: Vec<String>,
    },
    /// A requested concrete Platform does not exist or is incompatible.
    Requested(String),
}

impl fmt::Display for ResolveError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Catalog { path, source } => {
                write!(
                    formatter,
                    "failed to read Platform catalog `{}`: {source}",
                    path.display()
                )
            }
            Self::ManifestRead { path, source } => {
                write!(
                    formatter,
                    "failed to read Platform manifest `{}`: {source}",
                    path.display()
                )
            }
            Self::ManifestInvalid { path, message } => {
                write!(
                    formatter,
                    "invalid Platform manifest `{}`: {message}",
                    path.display()
                )
            }
            Self::NameMismatch {
                directory,
                declared,
            } => write!(
                formatter,
                "Platform directory `{directory}` declares Platform `{declared}`"
            ),
            Self::NoMatch(request) => write!(formatter, "no Platform matches {request}"),
            Self::Ambiguous { request, platforms } => write!(
                formatter,
                "multiple Platforms match {request}: {}",
                platforms.join(", ")
            ),
            Self::Requested(name) => {
                write!(
                    formatter,
                    "requested Platform `{name}` is missing or incompatible"
                )
            }
        }
    }
}

impl std::error::Error for ResolveError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Catalog { source, .. } | Self::ManifestRead { source, .. } => Some(source),
            _ => None,
        }
    }
}

/// Discovers the Platform matching one Cargo target.
///
/// # Errors
///
/// Returns [`ResolveError`] for an invalid catalog, no match, multiple matches,
/// or an incompatible explicit request.
pub fn resolve_platform(
    workspace: &Path,
    target: PlatformTarget<'_>,
    requested: Option<&str>,
) -> Result<PlatformDefinition, ResolveError> {
    let platforms = discover_platforms(workspace)?;
    if let Some(name) = requested {
        return platforms
            .into_iter()
            .find(|platform| platform.name == name && matches_target(&platform.selection, target))
            .ok_or_else(|| ResolveError::Requested(name.to_owned()));
    }
    unique_match(
        platforms
            .into_iter()
            .filter(|platform| matches_target(&platform.selection, target))
            .collect(),
        format!("Cargo target `{}`", target.triple),
    )
}

/// Discovers the Platform matching a selected Board's chip and toolchain target.
///
/// # Errors
///
/// Returns [`ResolveError`] for an invalid catalog, no match, or multiple
/// matching Platforms.
pub fn resolve_board_platform(
    workspace: &Path,
    chip: &str,
    toolchain_target: Option<&str>,
) -> Result<PlatformDefinition, ResolveError> {
    let platforms = discover_platforms(workspace)?;
    unique_match(
        platforms
            .into_iter()
            .filter(|platform| {
                platform
                    .selection
                    .board_chips
                    .iter()
                    .any(|pattern| wildcard_matches(pattern, chip))
                    && toolchain_target.is_none_or(|target| {
                        platform.selection.targets.iter().any(|selector| {
                            selector
                                .triple
                                .as_deref()
                                .is_some_and(|pattern| wildcard_matches(pattern, target))
                        })
                    })
            })
            .collect(),
        match toolchain_target {
            Some(target) => format!("Board chip `{chip}` with target `{target}`"),
            None => format!("Board chip `{chip}`"),
        },
    )
}

/// Discovers and validates every self-described Platform in stable directory order.
///
/// # Errors
///
/// Returns [`ResolveError`] when the Platform catalog or one of its manifests is invalid.
pub fn discover_platforms(workspace: &Path) -> Result<Vec<PlatformDefinition>, ResolveError> {
    let catalog = workspace.join("platforms");
    let entries = fs::read_dir(&catalog).map_err(|source| ResolveError::Catalog {
        path: catalog.clone(),
        source,
    })?;
    let mut manifests = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|source| ResolveError::Catalog {
            path: catalog.clone(),
            source,
        })?;
        if !entry
            .file_type()
            .map_err(|source| ResolveError::Catalog {
                path: entry.path(),
                source,
            })?
            .is_dir()
        {
            continue;
        }
        let manifest = entry.path().join("platform.yml");
        if manifest.is_file() {
            manifests.push(manifest);
        }
    }
    manifests.sort();
    manifests.into_iter().map(read_platform).collect()
}

fn read_platform(path: PathBuf) -> Result<PlatformDefinition, ResolveError> {
    let yaml = fs::read_to_string(&path).map_err(|source| ResolveError::ManifestRead {
        path: path.clone(),
        source,
    })?;
    let mut documents = yaml_peg::serde::from_str::<PlatformDocument>(&yaml).map_err(|error| {
        ResolveError::ManifestInvalid {
            path: path.clone(),
            message: error.to_string(),
        }
    })?;
    if documents.len() != 1 {
        return Err(ResolveError::ManifestInvalid {
            path,
            message: format!("expected one YAML document, found {}", documents.len()),
        });
    }
    let document = documents
        .pop()
        .ok_or_else(|| ResolveError::ManifestInvalid {
            path: path.clone(),
            message: String::from("manifest is empty"),
        })?;
    let directory =
        path.parent()
            .map(Path::to_path_buf)
            .ok_or_else(|| ResolveError::ManifestInvalid {
                path: path.clone(),
                message: String::from("manifest has no parent directory"),
            })?;
    let directory_name = directory
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| ResolveError::ManifestInvalid {
            path: path.clone(),
            message: String::from("Platform directory name is not UTF-8"),
        })?;
    if directory_name != document.name {
        return Err(ResolveError::NameMismatch {
            directory: directory_name.to_owned(),
            declared: document.name,
        });
    }
    validate_identifier(&document.crate_name, "crate", &path)?;
    validate_identifier(&document.type_name, "type", &path)?;
    if document.package.is_empty() || document.selection.board_chips.is_empty() {
        return Err(ResolveError::ManifestInvalid {
            path,
            message: String::from("package and selection.board-chips must not be empty"),
        });
    }
    if document
        .selection
        .features_by_chip
        .iter()
        .any(|(chip, features)| {
            chip.trim().is_empty()
                || features.is_empty()
                || features.iter().any(|feature| feature.trim().is_empty())
        })
    {
        return Err(ResolveError::ManifestInvalid {
            path,
            message: String::from(
                "selection.features-by-chip requires non-empty chips and Cargo features",
            ),
        });
    }
    if document
        .selection
        .targets
        .iter()
        .any(|target| target.rustflags.iter().any(|flag| flag.trim().is_empty()))
    {
        return Err(ResolveError::ManifestInvalid {
            path,
            message: String::from("selection.targets.rustflags must not contain empty flags"),
        });
    }
    if document
        .selection
        .targets
        .iter()
        .any(|target| !target.rustflags.is_empty() && target.triple.is_none())
    {
        return Err(ResolveError::ManifestInvalid {
            path,
            message: String::from("selection.targets.rustflags require a triple selector"),
        });
    }
    Ok(PlatformDefinition {
        name: document.name,
        package: document.package,
        crate_name: document.crate_name,
        type_name: document.type_name,
        directory,
        selection: document.selection,
        system_image: document.system_image,
        application: document.application,
    })
}

fn validate_identifier(value: &str, field: &str, path: &Path) -> Result<(), ResolveError> {
    let mut bytes = value.bytes();
    if bytes
        .next()
        .is_some_and(|first| first.is_ascii_alphabetic() || first == b'_')
        && bytes.all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
    {
        Ok(())
    } else {
        Err(ResolveError::ManifestInvalid {
            path: path.to_owned(),
            message: format!("invalid Rust identifier in `{field}`: `{value}`"),
        })
    }
}

fn matches_target(selection: &SelectionConfig, target: PlatformTarget<'_>) -> bool {
    selection.targets.iter().any(|selector| {
        let has_constraint =
            selector.triple.is_some() || selector.os.is_some() || selector.arch.is_some();
        has_constraint
            && selector
                .triple
                .as_deref()
                .is_none_or(|pattern| wildcard_matches(pattern, target.triple))
            && selector
                .os
                .as_deref()
                .is_none_or(|pattern| wildcard_matches(pattern, target.os))
            && selector
                .arch
                .as_deref()
                .is_none_or(|pattern| wildcard_matches(pattern, target.arch))
    })
}

fn unique_match(
    mut platforms: Vec<PlatformDefinition>,
    request: String,
) -> Result<PlatformDefinition, ResolveError> {
    match platforms.len() {
        0 => Err(ResolveError::NoMatch(request)),
        1 => platforms.pop().ok_or(ResolveError::NoMatch(request)),
        _ => Err(ResolveError::Ambiguous {
            request,
            platforms: platforms
                .into_iter()
                .map(|platform| platform.name)
                .collect(),
        }),
    }
}

fn wildcard_matches(pattern: &str, value: &str) -> bool {
    if pattern == "*" {
        return true;
    }
    let parts = pattern.split('*').collect::<Vec<_>>();
    if parts.len() == 1 {
        return pattern == value;
    }
    let mut remaining = value;
    for (index, part) in parts.iter().enumerate() {
        if part.is_empty() {
            continue;
        }
        if index == 0 && !pattern.starts_with('*') {
            let Some(tail) = remaining.strip_prefix(part) else {
                return false;
            };
            remaining = tail;
            continue;
        }
        let Some(position) = remaining.find(part) else {
            return false;
        };
        remaining = &remaining[position + part.len()..];
    }
    pattern.ends_with('*') || remaining.is_empty()
}
