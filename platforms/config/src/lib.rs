//! Filesystem discovery and host-side resolution of Barracuda Platforms.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

use serde::Deserialize;

/// One hardware resource form implemented by a Platform HAL.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum HalBinding {
    /// Digital input construction.
    DigitalInput,
    /// Digital output construction.
    DigitalOutput,
    /// Runtime-configurable GPIO construction.
    Gpio,
    /// Exclusive SPI-device construction.
    SpiDevice,
    /// Exclusively owned SPI-bus construction.
    SpiBus,
    /// Data-only SPI waveform construction with a Platform-owned controller.
    SpiOutput,
    /// MIPI DSI display-host construction.
    DsiHost,
    /// MIPI CSI camera-host construction.
    MipiCsi,
    /// SDMMC block-device construction.
    SdmmcDevice,
    /// I2C-device construction.
    I2cDevice,
    /// Parallel camera receiver construction.
    CameraCapture,
    /// Full-duplex I2S stream construction.
    I2sStream,
}

/// One ADC channel route owned by a Platform ADC controller pool.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RuntimeAdcChannel {
    channel: String,
    pin: String,
}

impl RuntimeAdcChannel {
    /// Returns the vendor HAL channel identifier.
    #[must_use]
    pub fn channel(&self) -> &str {
        &self.channel
    }

    /// Returns the physical pin routed to this channel.
    #[must_use]
    pub fn pin(&self) -> &str {
        &self.pin
    }
}

/// One Platform-owned ADC controller and its statically known channel routes.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RuntimeAdcController {
    controller: String,
    channels: Vec<RuntimeAdcChannel>,
}

impl RuntimeAdcController {
    /// Returns the vendor HAL controller singleton.
    #[must_use]
    pub fn controller(&self) -> &str {
        &self.controller
    }

    /// Returns channel routes supported by this controller.
    #[must_use]
    pub fn channels(&self) -> &[RuntimeAdcChannel] {
        &self.channels
    }
}

/// One Platform-owned PWM controller with timer and output-channel pools.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RuntimePwmController {
    controller: String,
    timers: Vec<String>,
    channels: Vec<String>,
}

impl RuntimePwmController {
    /// Returns the vendor HAL PWM controller singleton.
    #[must_use]
    pub fn controller(&self) -> &str {
        &self.controller
    }

    /// Returns the statically available timer identifiers.
    #[must_use]
    pub fn timers(&self) -> &[String] {
        &self.timers
    }

    /// Returns the statically available output-channel identifiers.
    #[must_use]
    pub fn channels(&self) -> &[String] {
        &self.channels
    }
}

/// One Platform-owned I2S controller with DMA and bounded buffer resources.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RuntimeI2sController {
    controller: String,
    #[serde(rename = "dma-channels")]
    dma_channels: Vec<String>,
    #[serde(rename = "dma-buffer-bytes")]
    dma_buffer_bytes: usize,
}

impl RuntimeI2sController {
    /// Returns the vendor HAL I2S controller singleton.
    #[must_use]
    pub fn controller(&self) -> &str {
        &self.controller
    }

    /// Returns DMA channels reserved for this runtime stream.
    #[must_use]
    pub fn dma_channels(&self) -> &[String] {
        &self.dma_channels
    }

    /// Returns the capacity of each statically allocated DMA direction buffer.
    #[must_use]
    pub const fn dma_buffer_bytes(&self) -> usize {
        self.dma_buffer_bytes
    }
}

/// Hardware construction capabilities implemented alongside one Platform.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct HalConfig {
    #[serde(default)]
    bindings: Vec<HalBinding>,
    #[serde(default, rename = "runtime-i2c-controllers")]
    runtime_i2c_controllers: Vec<String>,
    #[serde(default, rename = "runtime-spi-controllers")]
    runtime_spi_controllers: Vec<String>,
    #[serde(default, rename = "runtime-uart-controllers")]
    runtime_uart_controllers: Vec<String>,
    #[serde(default, rename = "runtime-adc-controllers")]
    runtime_adc_controllers: Vec<RuntimeAdcController>,
    #[serde(default, rename = "runtime-pwm-controllers")]
    runtime_pwm_controllers: Vec<RuntimePwmController>,
    #[serde(default, rename = "runtime-i2s-controllers")]
    runtime_i2s_controllers: Vec<RuntimeI2sController>,
}

impl HalConfig {
    /// Returns whether this Platform can construct the requested HAL binding.
    #[must_use]
    pub fn supports(&self, binding: HalBinding) -> bool {
        self.bindings.contains(&binding)
    }

    /// Returns the statically supported binding forms.
    #[must_use]
    pub fn bindings(&self) -> &[HalBinding] {
        &self.bindings
    }

    /// Returns controller singletons available for runtime I2C routing.
    #[must_use]
    pub fn runtime_i2c_controllers(&self) -> &[String] {
        &self.runtime_i2c_controllers
    }

    /// Returns controller singletons available for runtime SPI routing.
    #[must_use]
    pub fn runtime_spi_controllers(&self) -> &[String] {
        &self.runtime_spi_controllers
    }

    /// Returns controller singletons available for runtime UART routing.
    #[must_use]
    pub fn runtime_uart_controllers(&self) -> &[String] {
        &self.runtime_uart_controllers
    }

    /// Returns ADC controllers and their statically routed channels.
    #[must_use]
    pub fn runtime_adc_controllers(&self) -> &[RuntimeAdcController] {
        &self.runtime_adc_controllers
    }

    /// Returns PWM controllers with their timer and channel pools.
    #[must_use]
    pub fn runtime_pwm_controllers(&self) -> &[RuntimePwmController] {
        &self.runtime_pwm_controllers
    }

    /// Returns I2S controllers with DMA channels and bounded buffers.
    #[must_use]
    pub fn runtime_i2s_controllers(&self) -> &[RuntimeI2sController] {
        &self.runtime_i2s_controllers
    }

    fn validate(&self, path: &Path) -> Result<(), ResolveError> {
        let mut controllers = BTreeSet::new();
        for controller in self
            .runtime_i2c_controllers
            .iter()
            .chain(self.runtime_spi_controllers.iter())
            .chain(self.runtime_uart_controllers.iter())
        {
            validate_identifier(controller, "runtime controller", path)?;
            if !controllers.insert(controller.as_str()) {
                return Err(invalid_hal(
                    path,
                    "a runtime controller is declared more than once",
                ));
            }
        }

        for adc in &self.runtime_adc_controllers {
            validate_identifier(&adc.controller, "runtime ADC controller", path)?;
            if !controllers.insert(adc.controller.as_str()) {
                return Err(invalid_hal(
                    path,
                    "a runtime controller is declared more than once",
                ));
            }
            if adc.channels.is_empty() {
                return Err(invalid_hal(
                    path,
                    "a runtime ADC controller has no channels",
                ));
            }
            let mut channels = BTreeSet::new();
            for channel in &adc.channels {
                validate_identifier(&channel.channel, "runtime ADC channel", path)?;
                validate_identifier(&channel.pin, "runtime ADC pin", path)?;
                if !channels.insert(channel.channel.as_str()) {
                    return Err(invalid_hal(path, "a runtime ADC channel is duplicated"));
                }
            }
        }

        for pwm in &self.runtime_pwm_controllers {
            validate_identifier(&pwm.controller, "runtime PWM controller", path)?;
            if !controllers.insert(pwm.controller.as_str()) {
                return Err(invalid_hal(
                    path,
                    "a runtime controller is declared more than once",
                ));
            }
            if pwm.timers.is_empty() || pwm.channels.is_empty() {
                return Err(invalid_hal(
                    path,
                    "a runtime PWM controller requires timers and channels",
                ));
            }
            let mut timers = BTreeSet::new();
            for timer in &pwm.timers {
                validate_identifier(timer, "runtime PWM timer", path)?;
                if !timers.insert(timer.as_str()) {
                    return Err(invalid_hal(path, "a runtime PWM timer is duplicated"));
                }
            }
            let mut channels = BTreeSet::new();
            for channel in &pwm.channels {
                validate_identifier(channel, "runtime PWM channel", path)?;
                if !channels.insert(channel.as_str()) {
                    return Err(invalid_hal(path, "a runtime PWM channel is duplicated"));
                }
            }
        }

        let mut dma_channels = BTreeSet::new();
        for i2s in &self.runtime_i2s_controllers {
            validate_identifier(&i2s.controller, "runtime I2S controller", path)?;
            if !controllers.insert(i2s.controller.as_str()) {
                return Err(invalid_hal(
                    path,
                    "a runtime controller is declared more than once",
                ));
            }
            if i2s.dma_channels.is_empty() || i2s.dma_buffer_bytes == 0 {
                return Err(invalid_hal(
                    path,
                    "a runtime I2S controller requires DMA channels and a nonzero buffer",
                ));
            }
            for dma in &i2s.dma_channels {
                validate_identifier(dma, "runtime I2S DMA channel", path)?;
                if !dma_channels.insert(dma.as_str()) {
                    return Err(invalid_hal(path, "a runtime I2S DMA channel is duplicated"));
                }
            }
        }
        Ok(())
    }
}

fn invalid_hal(path: &Path, message: &str) -> ResolveError {
    ResolveError::ManifestInvalid {
        path: path.to_owned(),
        message: message.to_owned(),
    }
}

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
    hal: HalConfig,
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

    /// Returns hardware construction implemented by this Platform.
    #[must_use]
    pub const fn hal(&self) -> &HalConfig {
        &self.hal
    }

    /// Returns Platform-owned Cargo features needed by one supported chip.
    #[must_use]
    pub fn cargo_features_for_chip(&self, chip: &str) -> &[String] {
        self.selection
            .features_by_chip
            .get(chip)
            .map_or(&[], Vec::as_slice)
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
    #[serde(default)]
    hal: HalConfig,
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
    document.hal.validate(&path)?;
    Ok(PlatformDefinition {
        name: document.name,
        package: document.package,
        crate_name: document.crate_name,
        type_name: document.type_name,
        directory,
        selection: document.selection,
        system_image: document.system_image,
        application: document.application,
        hal: document.hal,
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
