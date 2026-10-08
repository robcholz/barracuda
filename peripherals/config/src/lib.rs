//! Peripheral implementation manifest loading and Board composition validation.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs, io,
    path::{Path, PathBuf},
};

use barracuda_board_config::{BoardDefinition, PeripheralDefinition, PeripheralParameter};
use barracuda_platform_config::{
    HalBinding, PlatformDefinition, RuntimeAdcController, RuntimeI2sController,
    RuntimePwmController,
};
use serde::Deserialize;

/// Peripheral implementation manifest schema version supported by this toolchain.
pub const PERIPHERAL_API_VERSION: u16 = 1;

/// Peripheral API identifiers owned by `peripherals/api` and understood by Board
/// resource generation.
pub const PERIPHERAL_APIS: &[&str] = &[
    "audio-codec",
    "buttons",
    "camera",
    "display",
    "indicator",
    "inertial-measurement",
    "led-strip",
    "power-monitor",
    "real-time-clock",
    "removable-storage",
    "touch",
];

/// All convention-discovered peripheral implementation manifests, indexed by stable ID.
#[derive(Debug)]
pub struct PeripheralCatalog {
    implementations: BTreeMap<String, PeripheralImplementationDefinition>,
}

impl PeripheralCatalog {
    /// Finds an implementation by the stable ID used in `board.yml`.
    #[must_use]
    pub fn implementation(&self, id: &str) -> Option<&PeripheralImplementationDefinition> {
        self.implementations.get(id)
    }

    /// Iterates implementations in stable ID order.
    pub fn implementations(&self) -> impl Iterator<Item = &PeripheralImplementationDefinition> {
        self.implementations.values()
    }
}

/// One validated `peripherals/impl/<peripheral>/<id>/peripheral.yml` manifest.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PeripheralImplementationDefinition {
    id: String,
    #[serde(rename = "api-version")]
    api_version: u16,
    peripheral: String,
    implementation: RustImplementation,
    #[serde(default)]
    bindings: BTreeMap<String, BindingSchema>,
    #[serde(default)]
    parameters: BTreeMap<String, ParameterSchema>,
    #[serde(skip)]
    directory: PathBuf,
}

impl PeripheralImplementationDefinition {
    /// Returns the stable ID referenced by Boards.
    #[must_use]
    pub fn id(&self) -> &str {
        &self.id
    }

    /// Returns the semantic peripheral API implemented by this implementation.
    #[must_use]
    pub fn peripheral(&self) -> &str {
        &self.peripheral
    }

    /// Returns the Rust mapping owned by this implementation.
    #[must_use]
    pub const fn implementation(&self) -> &RustImplementation {
        &self.implementation
    }

    /// Returns the implementation directory containing this manifest.
    #[must_use]
    pub fn directory(&self) -> &Path {
        &self.directory
    }

    /// Finds one binding role schema.
    #[must_use]
    pub fn binding(&self, role: &str) -> Option<&BindingSchema> {
        self.bindings.get(role)
    }

    /// Iterates binding schemas in stable role-name order.
    pub fn bindings(&self) -> impl Iterator<Item = (&str, &BindingSchema)> {
        self.bindings
            .iter()
            .map(|(name, schema)| (name.as_str(), schema))
    }

    /// Finds one parameter schema.
    #[must_use]
    pub fn parameter(&self, name: &str) -> Option<&ParameterSchema> {
        self.parameters.get(name)
    }
}

/// Cargo and Rust path used only by static code generation.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RustImplementation {
    package: String,
    #[serde(rename = "crate")]
    crate_name: String,
    factory: String,
    #[serde(rename = "bindings-expression")]
    bindings_expression: String,
    #[serde(rename = "config-expression")]
    config_expression: String,
}

impl RustImplementation {
    /// Returns the Cargo package selected as a dependency.
    #[must_use]
    pub fn package(&self) -> &str {
        &self.package
    }

    /// Returns the Rust crate identifier used in generated source.
    #[must_use]
    pub fn crate_name(&self) -> &str {
        &self.crate_name
    }

    /// Returns the implementation-owned factory type template.
    #[must_use]
    pub fn factory(&self) -> &str {
        &self.factory
    }

    /// Returns the implementation-owned bindings expression template.
    #[must_use]
    pub fn bindings_expression(&self) -> &str {
        &self.bindings_expression
    }

    /// Returns the implementation-owned configuration expression template.
    #[must_use]
    pub fn config_expression(&self) -> &str {
        &self.config_expression
    }
}

/// Initial electrical level selected before enabling a digital output.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum InitialOutputLevel {
    /// Drive low before handing the output to its peripheral implementation.
    Low,
    /// Drive high before handing the output to its peripheral implementation.
    High,
}

/// Implementation-owned mapping from one parameter to a safe initial output level.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct InitialOutputFromParameter {
    parameter: String,
    values: BTreeMap<String, InitialOutputLevel>,
}

impl InitialOutputFromParameter {
    /// Returns the implementation parameter that determines the initial level.
    #[must_use]
    pub fn parameter(&self) -> &str {
        &self.parameter
    }

    /// Returns the initial level mapped from one serialized parameter value.
    #[must_use]
    pub fn level_for(&self, value: &str) -> Option<InitialOutputLevel> {
        self.values.get(value).copied()
    }
}

/// Hardware resource category accepted by one Implementation binding.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum BindingKind {
    /// A concrete digital input pin.
    DigitalInput,
    /// A concrete digital output pin.
    DigitalOutput,
    /// One PWM output channel.
    Pwm,
    /// One addressed I2C device.
    I2cDevice,
    /// One selected SPI device.
    SpiDevice,
    /// One selected four-data-line half-duplex SPI device.
    QuadSpiDevice,
    /// One exclusively owned SPI controller without chip select.
    SpiBus,
    /// One data-only SPI waveform output using a Platform-owned controller.
    SpiOutput,
    /// One parallel output bus.
    ParallelOutput,
    /// One MIPI DSI host.
    DsiHost,
    /// One MIPI CSI camera host.
    MipiCsi,
    /// One fixed SDMMC block device.
    SdmmcDevice,
    /// One parallel camera receiver.
    CameraCapture,
    /// One full-duplex I2S stream.
    I2sStream,
    /// One fixed capacitive-touch channel group.
    CapacitiveTouch,
    /// A semantic power-control binding.
    PowerControl,
    /// A semantic brightness-control binding.
    BrightnessControl,
}

/// Schema for one named, move-only hardware binding.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct BindingSchema {
    kind: BindingKind,
    #[serde(default = "required_by_default")]
    required: bool,
    #[serde(default)]
    initial: Option<InitialOutputLevel>,
    #[serde(default, rename = "initial-from")]
    initial_from: Option<InitialOutputFromParameter>,
}

impl BindingSchema {
    /// Returns the hardware resource category.
    #[must_use]
    pub const fn kind(&self) -> BindingKind {
        self.kind
    }

    /// Returns whether the Board must supply this binding.
    #[must_use]
    pub const fn required(&self) -> bool {
        self.required
    }

    /// Returns the fixed safe initialization level, when declared.
    #[must_use]
    pub const fn initial(&self) -> Option<InitialOutputLevel> {
        self.initial
    }

    /// Returns the parameter-based safe initialization mapping, when declared.
    #[must_use]
    pub const fn initial_from(&self) -> Option<&InitialOutputFromParameter> {
        self.initial_from.as_ref()
    }
}

const fn required_by_default() -> bool {
    true
}

/// Portable value category accepted by one Implementation parameter.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum ParameterType {
    /// Boolean value.
    Boolean,
    /// Signed integer value.
    Integer,
    /// Arbitrary string value.
    String,
    /// String constrained to the schema's `values` list.
    Enum,
}

/// Schema for one Implementation initialization parameter.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ParameterSchema {
    #[serde(rename = "type")]
    parameter_type: ParameterType,
    #[serde(default)]
    required: bool,
    #[serde(default)]
    values: Vec<String>,
    #[serde(default)]
    default: Option<PeripheralParameter>,
    #[serde(default, rename = "rust-values")]
    rust_values: BTreeMap<String, String>,
}

impl ParameterSchema {
    /// Returns the portable value category.
    #[must_use]
    pub const fn parameter_type(&self) -> ParameterType {
        self.parameter_type
    }

    /// Returns whether the Board must explicitly supply this parameter.
    #[must_use]
    pub const fn required(&self) -> bool {
        self.required
    }

    /// Returns the optional default supplied by the Implementation.
    #[must_use]
    pub const fn default(&self) -> Option<&PeripheralParameter> {
        self.default.as_ref()
    }

    fn rust_value(&self, value: &str) -> Option<&str> {
        self.rust_values.get(value).map(String::as_str)
    }
}

/// A Board peripheral paired with its validated implementation manifest.
#[derive(Debug)]
pub struct ResolvedPeripheral<'a> {
    name: &'a str,
    definition: &'a PeripheralDefinition,
    implementation: &'a PeripheralImplementationDefinition,
}

impl<'a> ResolvedPeripheral<'a> {
    /// Returns the Board-level peripheral name.
    #[must_use]
    pub fn name(&self) -> &str {
        self.name
    }

    /// Returns the resolved implementation definition.
    #[must_use]
    pub const fn implementation(&self) -> &PeripheralImplementationDefinition {
        self.implementation
    }

    /// Returns one validated chip-native binding identifier.
    #[must_use]
    pub fn binding(&self, role: &str) -> Option<&str> {
        self.definition.binding(role)
    }

    /// Returns one validated Board parameter, or its implementation-owned default.
    #[must_use]
    pub fn parameter(&self, name: &str) -> Option<&PeripheralParameter> {
        self.definition.parameter(name).or_else(|| {
            self.implementation
                .parameter(name)
                .and_then(ParameterSchema::default)
        })
    }
}

/// Fully resolved static peripheral composition for one Board.
#[derive(Debug)]
pub struct ResolvedBoard<'a> {
    peripherals: Vec<ResolvedPeripheral<'a>>,
}

impl<'a> ResolvedBoard<'a> {
    /// Finds a peripheral by its Board-level name.
    #[must_use]
    pub fn peripheral(&self, name: &str) -> Option<&ResolvedPeripheral<'a>> {
        self.peripherals
            .iter()
            .find(|peripheral| peripheral.name == name)
    }

    /// Iterates peripherals in stable Board-name order.
    pub fn peripherals(&self) -> impl Iterator<Item = &ResolvedPeripheral<'a>> {
        self.peripherals.iter()
    }
}

/// Failure while discovering or validating implementation manifests.
#[derive(Debug, thiserror::Error)]
pub enum CatalogError {
    /// The implementation catalog directory could not be read.
    #[error("failed to read peripheral implementation catalog `{path}`: {source}")]
    Io {
        /// Path that failed.
        path: PathBuf,
        /// Underlying filesystem failure.
        #[source]
        source: io::Error,
    },
    /// An implementation manifest has invalid YAML or structure.
    #[error("invalid peripheral implementation manifest `{path}`: {message}")]
    Manifest {
        /// Manifest path.
        path: PathBuf,
        /// Parser or validation detail.
        message: String,
    },
    /// Directory name and stable implementation ID differ.
    #[error("peripheral implementation directory `{directory}` declares ID `{declared}`")]
    IdMismatch {
        /// Directory component.
        directory: String,
        /// Manifest ID.
        declared: String,
    },
    /// More than one manifest declares the same stable ID.
    #[error("duplicate peripheral implementation ID `{id}`")]
    DuplicateId {
        /// Duplicated ID.
        id: String,
    },
    /// An implementation names no stable API registered by `peripherals/api`.
    #[error(
        "peripheral implementation `{implementation}` references unregistered peripheral API `{peripheral}`; add the API trait before its implementation"
    )]
    UnregisteredPeripheralApi {
        /// Implementation ID.
        implementation: String,
        /// Unregistered API identifier.
        peripheral: String,
    },
}

/// Failure while resolving a Board against implementation-owned schemas.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ResolveError {
    /// Board references an undiscovered stable implementation ID.
    #[error("Board peripheral `{peripheral}` references unknown implementation `{implementation}`")]
    UnknownImplementation {
        /// Board-level peripheral name.
        peripheral: String,
        /// Unknown implementation ID.
        implementation: String,
    },
    /// A required Implementation binding is missing.
    #[error("Board peripheral `{peripheral}` is missing Implementation binding `{binding}`")]
    MissingBinding {
        /// Board-level peripheral name.
        peripheral: String,
        /// Missing binding role.
        binding: String,
    },
    /// Board supplies a binding absent from the Implementation schema.
    #[error("Board peripheral `{peripheral}` supplies unknown Implementation binding `{binding}`")]
    UnknownBinding {
        /// Board-level peripheral name.
        peripheral: String,
        /// Unknown binding role.
        binding: String,
    },
    /// A required Implementation parameter is missing.
    #[error("Board peripheral `{peripheral}` is missing Implementation parameter `{parameter}`")]
    MissingParameter {
        /// Board-level peripheral name.
        peripheral: String,
        /// Missing parameter name.
        parameter: String,
    },
    /// Board supplies a parameter absent from the Implementation schema.
    #[error(
        "Board peripheral `{peripheral}` supplies unknown Implementation parameter `{parameter}`"
    )]
    UnknownParameter {
        /// Board-level peripheral name.
        peripheral: String,
        /// Unknown parameter name.
        parameter: String,
    },
    /// A Board parameter does not satisfy the Implementation schema.
    #[error("Board peripheral `{peripheral}` has invalid Implementation parameter `{parameter}`")]
    InvalidParameter {
        /// Board-level peripheral name.
        peripheral: String,
        /// Invalid parameter name.
        parameter: String,
    },
    /// A protocol binding references no matching Board-internal resource.
    #[error(
        "Board peripheral `{peripheral}` binding `{binding}` references unknown {kind:?} resource `{resource}`"
    )]
    UnknownInternalResource {
        /// Board-level peripheral name.
        peripheral: String,
        /// Implementation-defined binding role.
        binding: String,
        /// Expected resource category.
        kind: BindingKind,
        /// Unknown Board-local resource name.
        resource: String,
    },
}

/// Failure while turning a resolved Board into monomorphized Rust source.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum GenerateError {
    /// The selected Platform does not implement a required HAL resource form.
    #[error("Platform `{platform}` HAL does not support {binding:?}")]
    UnsupportedHalBinding {
        /// Platform selected for the Board.
        platform: String,
        /// Required HAL resource form.
        binding: HalBinding,
    },
    /// A data-only SPI binding has no unreserved Platform controller.
    #[error("Platform has no unreserved SPI controller for a data-only output")]
    MissingPlatformSpiController,
    /// A Board selected multiple implementations for one singleton peripheral API.
    #[error("Board declares more than one `{peripheral}` peripheral")]
    DuplicatePeripheral {
        /// Duplicated peripheral API ID.
        peripheral: String,
    },
    /// A chip-native binding cannot be emitted as a Rust resource token.
    #[error("hardware identifier `{identifier}` is not a Rust resource token")]
    InvalidHardwareIdentifier {
        /// Invalid chip-native identifier.
        identifier: String,
    },
    /// A required, already schema-validated parameter is unavailable.
    #[error("resolved peripheral `{peripheral}` has no parameter `{parameter}`")]
    MissingResolvedParameter {
        /// Board-level peripheral name.
        peripheral: String,
        /// Parameter name.
        parameter: String,
    },
    /// A schema-valid value cannot be represented by the selected backend.
    #[error("resolved peripheral `{peripheral}` has unsupported value for `{parameter}`")]
    UnsupportedResolvedValue {
        /// Board-level peripheral name.
        peripheral: String,
        /// Parameter or resource field.
        parameter: String,
    },
    /// A generated peripheral currently requires a binding of another kind.
    #[error("Implementation `{implementation}` binding `{binding}` has unsupported kind {kind:?}")]
    UnsupportedBindingKind {
        /// Implementation ID.
        implementation: String,
        /// Binding role.
        binding: String,
        /// Unsupported category.
        kind: BindingKind,
    },
    /// A Implementation-owned composition template contains an unknown placeholder.
    #[error(
        "Implementation `{implementation}` composition template contains unknown placeholder `{placeholder}`"
    )]
    InvalidTemplate {
        /// Implementation whose template failed.
        implementation: String,
        /// Placeholder that could not be resolved.
        placeholder: String,
    },
    /// Two Board names normalize to the same generated Rust identifier.
    #[error(
        "Board hardware names `{first}` and `{second}` both generate Rust identifier `{identifier}`"
    )]
    IdentifierCollision {
        /// First Board name.
        first: String,
        /// Conflicting Board name.
        second: String,
        /// Generated identifier.
        identifier: String,
    },
}

/// Discovers `peripherals/impl/<peripheral>/<id>/peripheral.yml` implementations.
///
/// # Errors
///
/// Returns [`CatalogError`] for filesystem, YAML, identity, or schema failures.
pub fn load_catalog(workspace_root: &Path) -> Result<PeripheralCatalog, CatalogError> {
    let root = workspace_root.join("peripherals/impl");
    let mut implementations = BTreeMap::new();
    let peripheral_entries = match fs::read_dir(&root) {
        Ok(entries) => entries,
        Err(source) if source.kind() == io::ErrorKind::NotFound => {
            return Ok(PeripheralCatalog { implementations });
        }
        Err(source) => {
            return Err(CatalogError::Io {
                path: root.clone(),
                source,
            });
        }
    };
    for peripheral_entry in peripheral_entries {
        let peripheral_entry = peripheral_entry.map_err(|source| CatalogError::Io {
            path: root.clone(),
            source,
        })?;
        let file_type = peripheral_entry
            .file_type()
            .map_err(|source| CatalogError::Io {
                path: peripheral_entry.path(),
                source,
            })?;
        if !file_type.is_dir() {
            continue;
        }
        let Ok(peripheral) = peripheral_entry.file_name().into_string() else {
            continue;
        };
        let implementation_root = peripheral_entry.path();
        let implementation_entries =
            fs::read_dir(&implementation_root).map_err(|source| CatalogError::Io {
                path: implementation_root.clone(),
                source,
            })?;
        for implementation_entry in implementation_entries {
            let implementation_entry = implementation_entry.map_err(|source| CatalogError::Io {
                path: implementation_root.clone(),
                source,
            })?;
            let file_type =
                implementation_entry
                    .file_type()
                    .map_err(|source| CatalogError::Io {
                        path: implementation_entry.path(),
                        source,
                    })?;
            if !file_type.is_dir() {
                continue;
            }
            let Ok(directory) = implementation_entry.file_name().into_string() else {
                continue;
            };
            let path = implementation_entry.path().join("peripheral.yml");
            let yaml = match fs::read_to_string(&path) {
                Ok(yaml) => yaml,
                Err(source) if source.kind() == io::ErrorKind::NotFound => continue,
                Err(source) => return Err(CatalogError::Io { path, source }),
            };
            let mut implementation = parse_manifest(&path, &yaml)?;
            if !PERIPHERAL_APIS.contains(&implementation.peripheral.as_str()) {
                return Err(CatalogError::UnregisteredPeripheralApi {
                    implementation: implementation.id.clone(),
                    peripheral: implementation.peripheral.clone(),
                });
            }
            if implementation.id != directory {
                return Err(CatalogError::IdMismatch {
                    directory,
                    declared: implementation.id,
                });
            }
            if implementation.peripheral != peripheral {
                return Err(CatalogError::Manifest {
                    path,
                    message: format!(
                        "peripheral `{}` does not match implementation category `{peripheral}`",
                        implementation.peripheral
                    ),
                });
            }
            implementation.directory = implementation_entry.path();
            let id = implementation.id.clone();
            if implementations.insert(id.clone(), implementation).is_some() {
                return Err(CatalogError::DuplicateId { id });
            }
        }
    }
    Ok(PeripheralCatalog { implementations })
}

fn parse_manifest(
    path: &Path,
    yaml: &str,
) -> Result<PeripheralImplementationDefinition, CatalogError> {
    let mut documents = yaml_peg::serde::from_str::<PeripheralImplementationDefinition>(yaml)
        .map_err(|error| CatalogError::Manifest {
            path: path.to_owned(),
            message: error.to_string(),
        })?;
    if documents.len() != 1 {
        return Err(CatalogError::Manifest {
            path: path.to_owned(),
            message: format!("expected one YAML document, found {}", documents.len()),
        });
    }
    let implementation = documents.pop().ok_or_else(|| CatalogError::Manifest {
        path: path.to_owned(),
        message: String::from("expected one YAML document, found 0"),
    })?;
    validate_manifest(path, &implementation)?;
    Ok(implementation)
}

fn validate_manifest(
    path: &Path,
    implementation: &PeripheralImplementationDefinition,
) -> Result<(), CatalogError> {
    let valid_id = !implementation.id.is_empty()
        && implementation
            .id
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-');
    let valid_crate = !implementation.implementation.crate_name.is_empty()
        && implementation
            .implementation
            .crate_name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_');
    let valid_names = implementation
        .bindings
        .keys()
        .chain(implementation.parameters.keys())
        .all(|name| !name.trim().is_empty());
    let valid_enums = implementation
        .parameters
        .values()
        .all(|schema| schema.parameter_type != ParameterType::Enum || !schema.values.is_empty());
    if !valid_id
        || implementation.api_version != PERIPHERAL_API_VERSION
        || implementation.peripheral.trim().is_empty()
        || implementation.implementation.package.trim().is_empty()
        || !valid_crate
        || implementation.implementation.factory.trim().is_empty()
        || implementation
            .implementation
            .bindings_expression
            .trim()
            .is_empty()
        || implementation
            .implementation
            .config_expression
            .trim()
            .is_empty()
        || !valid_names
        || !valid_enums
    {
        return Err(CatalogError::Manifest {
            path: path.to_owned(),
            message: String::from(
                "manifest identity, API version, implementation, or schema is invalid",
            ),
        });
    }
    for template in [
        implementation.implementation.factory(),
        implementation.implementation.bindings_expression(),
        implementation.implementation.config_expression(),
    ] {
        validate_template_placeholders(implementation, template).map_err(|message| {
            CatalogError::Manifest {
                path: path.to_owned(),
                message,
            }
        })?;
    }
    for (name, schema) in &implementation.parameters {
        if let Some(default) = &schema.default
            && !parameter_matches(schema, default)
        {
            return Err(CatalogError::Manifest {
                path: path.to_owned(),
                message: format!("default for parameter `{name}` does not match its schema"),
            });
        }
        if !schema.rust_values.is_empty()
            && (schema.parameter_type != ParameterType::Enum
                || schema.rust_values.iter().any(|(value, rust)| {
                    rust.trim().is_empty()
                        || !schema.values.iter().any(|candidate| candidate == value)
                }))
        {
            return Err(CatalogError::Manifest {
                path: path.to_owned(),
                message: format!("Rust value mapping for parameter `{name}` is outside its enum"),
            });
        }
    }
    for (role, schema) in &implementation.bindings {
        let has_initial = schema.initial.is_some();
        let has_mapping = schema.initial_from.is_some();
        if (has_initial || has_mapping) && schema.kind != BindingKind::DigitalOutput
            || has_initial && has_mapping
        {
            return Err(CatalogError::Manifest {
                path: path.to_owned(),
                message: format!("binding `{role}` has an invalid output initialization policy"),
            });
        }
        if schema.kind == BindingKind::DigitalOutput && !has_initial && !has_mapping {
            return Err(CatalogError::Manifest {
                path: path.to_owned(),
                message: format!(
                    "digital output binding `{role}` must declare its safe initial level"
                ),
            });
        }
        if let Some(mapping) = &schema.initial_from {
            let Some(parameter) = implementation.parameter(&mapping.parameter) else {
                return Err(CatalogError::Manifest {
                    path: path.to_owned(),
                    message: format!(
                        "binding `{role}` maps unknown parameter `{}`",
                        mapping.parameter
                    ),
                });
            };
            let complete_mapping = match parameter.parameter_type {
                ParameterType::Enum => {
                    parameter
                        .values
                        .iter()
                        .all(|value| mapping.values.contains_key(value))
                        && mapping.values.keys().all(|value| {
                            parameter.values.iter().any(|candidate| candidate == value)
                        })
                }
                ParameterType::Boolean => {
                    mapping.values.len() == 2
                        && mapping.values.contains_key("true")
                        && mapping.values.contains_key("false")
                }
                ParameterType::Integer | ParameterType::String => false,
            };
            if !complete_mapping || (!parameter.required && parameter.default.is_none()) {
                return Err(CatalogError::Manifest {
                    path: path.to_owned(),
                    message: format!(
                        "binding `{role}` does not map every parameter value to an initial level"
                    ),
                });
            }
        }
    }
    Ok(())
}

fn validate_template_placeholders(
    implementation: &PeripheralImplementationDefinition,
    template: &str,
) -> Result<(), String> {
    let mut remaining = template;
    while let Some(start) = remaining.find("{{") {
        let placeholder_start = start + 2;
        let Some(end) = remaining[placeholder_start..].find("}}") else {
            return Err(format!(
                "Implementation `{}` has an unterminated composition placeholder",
                implementation.id()
            ));
        };
        let placeholder_end = placeholder_start + end;
        let placeholder = remaining[placeholder_start..placeholder_end].trim();
        let valid = matches!(placeholder, "crate" | "peripheral.name")
            || matches!(placeholder, "hal.delay.type" | "hal.delay.value")
            || placeholder
                .strip_prefix("parameter.")
                .is_some_and(|name| implementation.parameter(name).is_some())
            || placeholder
                .strip_prefix("binding.")
                .and_then(|value| value.rsplit_once('.'))
                .is_some_and(|(role, part)| {
                    matches!(part, "type" | "value") && implementation.binding(role).is_some()
                });
        if !valid {
            return Err(format!(
                "Implementation `{}` composition template contains unknown placeholder `{placeholder}`",
                implementation.id()
            ));
        }
        remaining = &remaining[placeholder_end + 2..];
    }
    Ok(())
}

/// Validates every Board peripheral against the referenced implementation schema.
///
/// # Errors
///
/// Returns [`ResolveError`] for unknown IDs, schema mismatches, missing values,
/// or values of the wrong type.
pub fn resolve_board<'a>(
    board: &'a BoardDefinition,
    catalog: &'a PeripheralCatalog,
) -> Result<ResolvedBoard<'a>, ResolveError> {
    let mut peripherals = Vec::with_capacity(board.peripheral_count());
    for (name, definition) in board.peripheral_devices() {
        let implementation = catalog
            .implementation(definition.implementation())
            .ok_or_else(|| ResolveError::UnknownImplementation {
                peripheral: name.to_owned(),
                implementation: definition.implementation().to_owned(),
            })?;
        for (role, schema) in implementation.bindings() {
            if schema.required() && definition.binding(role).is_none() {
                return Err(ResolveError::MissingBinding {
                    peripheral: name.to_owned(),
                    binding: role.to_owned(),
                });
            }
            if let Some(resource) = definition.binding(role)
                && ((schema.kind() == BindingKind::SpiDevice
                    && board.peripheral_io().spi_device(resource).is_none())
                    || (schema.kind() == BindingKind::QuadSpiDevice
                        && board.peripheral_io().quad_spi_device(resource).is_none())
                    || (schema.kind() == BindingKind::SpiBus
                        && board.peripheral_io().spi_bus(resource).is_none())
                    || (schema.kind() == BindingKind::SpiOutput
                        && board.peripheral_io().spi_output(resource).is_none())
                    || (schema.kind() == BindingKind::I2cDevice
                        && board.peripheral_io().i2c_device(resource).is_none())
                    || (schema.kind() == BindingKind::DsiHost
                        && board.peripheral_io().dsi_host(resource).is_none())
                    || (schema.kind() == BindingKind::MipiCsi
                        && board.peripheral_io().mipi_csi(resource).is_none())
                    || (schema.kind() == BindingKind::SdmmcDevice
                        && board.peripheral_io().sdmmc_device(resource).is_none())
                    || (schema.kind() == BindingKind::CameraCapture
                        && board.peripheral_io().camera_capture(resource).is_none())
                    || (schema.kind() == BindingKind::I2sStream
                        && board.peripheral_io().i2s_stream(resource).is_none())
                    || (schema.kind() == BindingKind::CapacitiveTouch
                        && board.peripheral_io().capacitive_touch(resource).is_none()))
            {
                return Err(ResolveError::UnknownInternalResource {
                    peripheral: name.to_owned(),
                    binding: role.to_owned(),
                    kind: schema.kind(),
                    resource: resource.to_owned(),
                });
            }
        }
        for (role, _) in definition.bindings() {
            if implementation.binding(role).is_none() {
                return Err(ResolveError::UnknownBinding {
                    peripheral: name.to_owned(),
                    binding: role.to_owned(),
                });
            }
        }
        for (parameter, schema) in &implementation.parameters {
            match definition.parameter(parameter) {
                Some(value) if !parameter_matches(schema, value) => {
                    return Err(ResolveError::InvalidParameter {
                        peripheral: name.to_owned(),
                        parameter: parameter.clone(),
                    });
                }
                None if schema.required && schema.default.is_none() => {
                    return Err(ResolveError::MissingParameter {
                        peripheral: name.to_owned(),
                        parameter: parameter.clone(),
                    });
                }
                Some(_) | None => {}
            }
        }
        for (parameter, _) in definition.parameters() {
            if implementation.parameter(parameter).is_none() {
                return Err(ResolveError::UnknownParameter {
                    peripheral: name.to_owned(),
                    parameter: parameter.to_owned(),
                });
            }
        }
        peripherals.push(ResolvedPeripheral {
            name,
            definition,
            implementation,
        });
    }
    Ok(ResolvedBoard { peripherals })
}

fn parameter_matches(schema: &ParameterSchema, value: &PeripheralParameter) -> bool {
    match (schema.parameter_type, value) {
        (ParameterType::Boolean, PeripheralParameter::Boolean(_))
        | (ParameterType::Integer, PeripheralParameter::Integer(_))
        | (ParameterType::String, PeripheralParameter::String(_)) => true,
        (ParameterType::Enum, PeripheralParameter::String(value)) => {
            schema.values.iter().any(|candidate| candidate == value)
        }
        _ => false,
    }
}

/// Renders the selected Board HAL from validated Board and Implementation manifests.
///
/// The source contains concrete Platform HAL and Implementation types. It performs no
/// runtime lookup and introduces no trait objects or heap allocation.
///
/// # Errors
///
/// Returns [`GenerateError`] when a binding or template cannot be emitted by
/// the static generator.
pub fn render_board_hal(
    board: &BoardDefinition,
    resolved: &ResolvedBoard<'_>,
) -> Result<String, GenerateError> {
    if !board.has_hardware_surface() {
        return Ok(String::from(
            "/// Board HAL selected independently from Platform.\n\
             pub type SelectedBoardHal = ::barracuda_board_hal::EmptyBoardHal;\n\n\
             #[doc(hidden)]\n\
             #[macro_export]\n\
             macro_rules! __barracuda_generated_board_bindings {\n\
                 ($hardware:ident) => { () };\n\
             }\n",
        ));
    }
    render_generic_hal(board, resolved, RuntimeResources::default())
}

/// Renders a selected Board HAL with the runtime controller pools supplied by
/// the independently selected Platform.
///
/// # Errors
///
/// Returns [`GenerateError`] when a binding or controller token cannot be
/// emitted by the static generator.
pub fn render_board_hal_for_platform(
    board: &BoardDefinition,
    resolved: &ResolvedBoard<'_>,
    platform: &PlatformDefinition,
) -> Result<String, GenerateError> {
    if !board.has_hardware_surface() {
        return render_board_hal(board, resolved);
    }
    render_generic_hal(
        board,
        resolved,
        RuntimeResources {
            i2c: platform.hal().runtime_i2c_controllers(),
            spi: platform.hal().runtime_spi_controllers(),
            uart: platform.hal().runtime_uart_controllers(),
            adc: platform.hal().runtime_adc_controllers(),
            pwm: platform.hal().runtime_pwm_controllers(),
            i2s: platform.hal().runtime_i2s_controllers(),
            peripheral_models: platform.hal().peripheral_models(),
        },
    )
}

/// Validates that a selected Platform HAL can construct every Board resource.
///
/// # Errors
///
/// Returns [`GenerateError::UnsupportedHalBinding`] when the Platform manifest
/// omits a binding form required by the Board or one of its Implementations.
pub fn validate_platform_hal(
    board: &BoardDefinition,
    resolved: &ResolvedBoard<'_>,
    platform: &PlatformDefinition,
) -> Result<(), GenerateError> {
    if board.exposed_io().pins().next().is_some() && !platform.hal().supports(HalBinding::Gpio) {
        return Err(GenerateError::UnsupportedHalBinding {
            platform: platform.name().to_owned(),
            binding: HalBinding::Gpio,
        });
    }
    if board.exposed_io().pins().next().is_some()
        && !platform.hal().runtime_i2c_controllers().is_empty()
        && !platform.hal().supports(HalBinding::I2cDevice)
    {
        return Err(GenerateError::UnsupportedHalBinding {
            platform: platform.name().to_owned(),
            binding: HalBinding::I2cDevice,
        });
    }
    if board.exposed_io().pins().next().is_some()
        && !platform.hal().runtime_spi_controllers().is_empty()
        && !platform.hal().supports(HalBinding::SpiBus)
    {
        return Err(GenerateError::UnsupportedHalBinding {
            platform: platform.name().to_owned(),
            binding: HalBinding::SpiBus,
        });
    }
    for peripheral in resolved.peripherals() {
        for (role, schema) in peripheral.implementation().bindings() {
            if peripheral.binding(role).is_some()
                && let Some(binding) = hal_binding(schema.kind())
                && !platform.hal().supports(binding)
            {
                return Err(GenerateError::UnsupportedHalBinding {
                    platform: platform.name().to_owned(),
                    binding,
                });
            }
        }
    }
    Ok(())
}

const fn hal_binding(kind: BindingKind) -> Option<HalBinding> {
    match kind {
        BindingKind::DigitalInput => Some(HalBinding::DigitalInput),
        BindingKind::DigitalOutput => Some(HalBinding::DigitalOutput),
        BindingKind::SpiDevice => Some(HalBinding::SpiDevice),
        BindingKind::QuadSpiDevice => Some(HalBinding::QuadSpiDevice),
        BindingKind::SpiBus => Some(HalBinding::SpiBus),
        BindingKind::SpiOutput => Some(HalBinding::SpiOutput),
        BindingKind::I2cDevice => Some(HalBinding::I2cDevice),
        BindingKind::DsiHost => Some(HalBinding::DsiHost),
        BindingKind::MipiCsi => Some(HalBinding::MipiCsi),
        BindingKind::SdmmcDevice => Some(HalBinding::SdmmcDevice),
        BindingKind::CameraCapture => Some(HalBinding::CameraCapture),
        BindingKind::I2sStream => Some(HalBinding::I2sStream),
        BindingKind::CapacitiveTouch => Some(HalBinding::CapacitiveTouch),
        _ => None,
    }
}

#[derive(Debug)]
struct RenderedPeripheral {
    name: String,
    field: String,
    alias: String,
    error_variant: String,
    peripheral_api: String,
    factory: String,
    bindings: String,
    config: String,
    declaration: String,
}

#[derive(Debug)]
struct RenderedI2cBus {
    local: String,
    controller: String,
    scl: String,
    sda: String,
    frequency_hz: u32,
    error_variant: String,
}

#[derive(Default)]
struct RenderState {
    raw_fields: Vec<(String, String)>,
    binding_errors: Vec<(String, String)>,
    identifiers: BTreeMap<String, String>,
    reserved_spi_controllers: BTreeSet<String>,
    i2c_buses: BTreeMap<String, RenderedI2cBus>,
}

#[derive(Clone, Copy, Default)]
struct RuntimeResources<'a> {
    i2c: &'a [String],
    spi: &'a [String],
    uart: &'a [String],
    adc: &'a [RuntimeAdcController],
    pwm: &'a [RuntimePwmController],
    i2s: &'a [RuntimeI2sController],
    peripheral_models: bool,
}

fn render_generic_hal(
    board: &BoardDefinition,
    resolved: &ResolvedBoard<'_>,
    resources: RuntimeResources<'_>,
) -> Result<String, GenerateError> {
    let RuntimeResources {
        i2c: runtime_i2c_controllers,
        spi: runtime_spi_controllers,
        uart: runtime_uart_controllers,
        adc: runtime_adc_controllers,
        pwm: runtime_pwm_controllers,
        i2s: runtime_i2s_controllers,
        peripheral_models,
    } = resources;
    let mut state = RenderState::default();
    let mut rendered = Vec::new();
    let mut primary_peripherals = BTreeSet::new();

    for peripheral in resolved.peripherals() {
        let field = checked_identifier(peripheral.name(), &mut state.identifiers)?;
        if !primary_peripherals.insert(peripheral.implementation().peripheral()) {
            return Err(GenerateError::DuplicatePeripheral {
                peripheral: peripheral.implementation().peripheral().to_owned(),
            });
        }

        let mut substitutions = BTreeMap::new();
        substitutions.insert(
            String::from("crate"),
            peripheral
                .implementation()
                .implementation()
                .crate_name()
                .to_owned(),
        );
        substitutions.insert(
            String::from("peripheral.name"),
            format!("{:?}", peripheral.name()),
        );
        substitutions.insert(
            String::from("hal.delay.type"),
            String::from("::barracuda_platform_selected::__platform::hal::PeripheralDelay"),
        );
        substitutions.insert(
            String::from("hal.delay.value"),
            String::from("::barracuda_platform_selected::__platform::hal::delay()"),
        );
        for (role, schema) in peripheral.implementation().bindings() {
            if let Some(resource) = peripheral.binding(role) {
                let mut rendered_binding = render_binding(
                    board,
                    peripheral,
                    role,
                    schema,
                    resource,
                    runtime_spi_controllers,
                    &mut state,
                )?;
                if !schema.required() {
                    rendered_binding.1 = format!("Some({})", rendered_binding.1);
                }
                substitutions.insert(format!("binding.{role}.type"), rendered_binding.0);
                substitutions.insert(format!("binding.{role}.value"), rendered_binding.1);
            } else if !schema.required() {
                substitutions.insert(
                    format!("binding.{role}.type"),
                    optional_binding_type(peripheral, role, schema.kind())?,
                );
                substitutions.insert(format!("binding.{role}.value"), String::from("None"));
            }
        }
        for (name, schema) in &peripheral.implementation().parameters {
            let value = peripheral.parameter(name).ok_or_else(|| {
                GenerateError::MissingResolvedParameter {
                    peripheral: peripheral.name().to_owned(),
                    parameter: name.clone(),
                }
            })?;
            substitutions.insert(
                format!("parameter.{name}"),
                render_parameter(peripheral, name, schema, value)?,
            );
        }

        let implementation = peripheral.implementation().implementation();
        let factory = render_template(peripheral, implementation.factory(), &substitutions)?;
        let bindings = render_template(
            peripheral,
            implementation.bindings_expression(),
            &substitutions,
        )?;
        let config = render_template(
            peripheral,
            implementation.config_expression(),
            &substitutions,
        )?;
        rendered.push(RenderedPeripheral {
            name: peripheral.name().to_owned(),
            alias: format!("{}Peripheral", pascal_identifier(peripheral.name())),
            error_variant: pascal_identifier(peripheral.name()),
            peripheral_api: peripheral.implementation().peripheral().to_owned(),
            field,
            factory,
            bindings,
            config,
            declaration: render_declaration(board, peripheral)?,
        });
    }

    let pin_count = board.exposed_io().pins().count();
    for (name, pin) in board.exposed_io().pins() {
        validate_hardware_identifier(pin.pin())?;
        let field = checked_identifier(&format!("pin_{name}"), &mut state.identifiers)?;
        state.raw_fields.push((
            field,
            format!(
                "::barracuda_platform_selected::__platform::hal::pin_binding_type!({})",
                pin.pin()
            ),
        ));
    }

    let runtime_i2c_controllers = if pin_count == 0 {
        Vec::new()
    } else {
        runtime_i2c_controllers
            .iter()
            .filter(|controller| !board.peripheral_io().uses_controller(controller))
            .filter(|controller| !state.reserved_spi_controllers.contains(*controller))
            .collect::<Vec<_>>()
    };
    let runtime_spi_controllers = if pin_count == 0 {
        Vec::new()
    } else {
        runtime_spi_controllers
            .iter()
            .filter(|controller| !board.peripheral_io().uses_controller(controller))
            .collect::<Vec<_>>()
    };
    let runtime_uart_controllers = if pin_count == 0 {
        Vec::new()
    } else {
        runtime_uart_controllers
            .iter()
            .filter(|controller| !board.peripheral_io().uses_controller(controller))
            .collect::<Vec<_>>()
    };
    let runtime_adc_controllers = if pin_count == 0 {
        Vec::new()
    } else {
        runtime_adc_controllers
            .iter()
            .filter(|resource| !board.peripheral_io().uses_controller(resource.controller()))
            .collect::<Vec<_>>()
    };
    let runtime_pwm_controllers = if pin_count == 0 {
        Vec::new()
    } else {
        runtime_pwm_controllers
            .iter()
            .filter(|resource| !board.peripheral_io().uses_controller(resource.controller()))
            .collect::<Vec<_>>()
    };
    let runtime_i2s_controllers = if pin_count == 0 {
        Vec::new()
    } else {
        runtime_i2s_controllers
            .iter()
            .filter(|resource| {
                !board.peripheral_io().uses_controller(resource.controller())
                    && !resource
                        .dma_channels()
                        .iter()
                        .any(|dma| board.peripheral_io().uses_dma(dma))
            })
            .collect::<Vec<_>>()
    };
    for controller in runtime_i2c_controllers
        .iter()
        .chain(runtime_spi_controllers.iter())
        .chain(runtime_uart_controllers.iter())
    {
        validate_hardware_identifier(controller)?;
        let field = checked_identifier(
            &format!("runtime_controller_{controller}"),
            &mut state.identifiers,
        )?;
        state.raw_fields.push((
            field,
            format!(
                "::barracuda_platform_selected::__platform::hal::controller_binding_type!({controller})"
            ),
        ));
    }
    for resource in &runtime_adc_controllers {
        validate_hardware_identifier(resource.controller())?;
        let field = checked_identifier(
            &format!("runtime_adc_controller_{}", resource.controller()),
            &mut state.identifiers,
        )?;
        state.raw_fields.push((
            field,
            format!(
                "::barracuda_platform_selected::__platform::hal::controller_binding_type!({})",
                resource.controller()
            ),
        ));
        for channel in resource.channels() {
            validate_hardware_identifier(channel.channel())?;
            validate_hardware_identifier(channel.pin())?;
        }
    }
    for resource in &runtime_pwm_controllers {
        validate_hardware_identifier(resource.controller())?;
        let field = checked_identifier(
            &format!("runtime_pwm_controller_{}", resource.controller()),
            &mut state.identifiers,
        )?;
        state.raw_fields.push((
            field,
            format!(
                "::barracuda_platform_selected::__platform::hal::controller_binding_type!({})",
                resource.controller()
            ),
        ));
        for timer in resource.timers() {
            validate_hardware_identifier(timer)?;
        }
        for channel in resource.channels() {
            validate_hardware_identifier(channel)?;
        }
    }
    for (index, resource) in runtime_i2s_controllers.iter().enumerate() {
        validate_hardware_identifier(resource.controller())?;
        let field = checked_identifier(
            &format!("runtime_i2s_controller_{}", resource.controller()),
            &mut state.identifiers,
        )?;
        state.raw_fields.push((
            field,
            format!(
                "::barracuda_platform_selected::__platform::hal::controller_binding_type!({})",
                resource.controller()
            ),
        ));
        for dma in resource.dma_channels() {
            validate_hardware_identifier(dma)?;
            let field =
                checked_identifier(&format!("runtime_i2s_dma_{dma}"), &mut state.identifiers)?;
            state.raw_fields.push((
                field,
                format!(
                    "::barracuda_platform_selected::__platform::hal::controller_binding_type!({dma})"
                ),
            ));
        }
        state.binding_errors.push((
            format!("RuntimeI2s{index}"),
            String::from("::barracuda_platform_selected::__platform::hal::RuntimeI2sResourceError"),
        ));
    }

    let mut source = String::from("mod generated_board_hal {\n");
    if !rendered.is_empty() || !state.binding_errors.is_empty() {
        source.push_str("use core::fmt;\n");
    }
    source.push_str("use ::barracuda_board_hal::{BoardHal, BoardHalInitResult, BoardResources");
    if !rendered.is_empty() {
        source.push_str(", PeripheralImplementation");
    }
    source
        .push_str("};\nuse ::embassy_executor::Spawner;\n\npub struct GeneratedBoardBindings {\n");
    for (field, ty) in &state.raw_fields {
        source.push_str(&format!("    {field}: {ty},\n"));
    }
    source.push_str(
        "}\n\nimpl GeneratedBoardBindings {\n    #[must_use]\n    #[allow(clippy::too_many_arguments)]\n    pub const fn new(\n",
    );
    for (field, ty) in &state.raw_fields {
        source.push_str(&format!("        {field}: {ty},\n"));
    }
    source.push_str("    ) -> Self {\n        Self {\n");
    for (field, _) in &state.raw_fields {
        source.push_str(&format!("            {field},\n"));
    }
    source.push_str("        }\n    }\n}\n\n");

    source.push_str(
        "#[doc(hidden)]\n#[macro_export]\nmacro_rules! __barracuda_generated_board_bindings {\n    ($peripherals:ident) => {\n        $crate::Bindings::new(\n",
    );
    for (_, ty) in &state.raw_fields {
        let Some((_, hardware)) = ty.rsplit_once("!(") else {
            return Err(GenerateError::InvalidHardwareIdentifier {
                identifier: ty.clone(),
            });
        };
        let Some(hardware) = hardware.strip_suffix(')') else {
            return Err(GenerateError::InvalidHardwareIdentifier {
                identifier: ty.clone(),
            });
        };
        source.push_str(&format!("            $peripherals.{hardware},\n"));
    }
    source.push_str("        )\n    };\n}\n\n");

    for peripheral in &rendered {
        source.push_str(&format!(
            "type {}Factory = {};\npub type {} = <{}Factory as PeripheralImplementation>::Peripheral;\n\n",
            peripheral.error_variant,
            peripheral.factory,
            peripheral.alias,
            peripheral.error_variant,
        ));
    }
    if rendered.is_empty() {
        source
            .push_str("pub type GeneratedPeripherals = ::barracuda_board_hal::NoPeripherals;\n\n");
    } else {
        source.push_str("pub struct GeneratedPeripherals {\n");
        for peripheral in &rendered {
            source.push_str(&format!(
                "    {}: Option<{}>,\n",
                peripheral.field, peripheral.alias
            ));
        }
        source.push_str("}\n\nimpl GeneratedPeripherals {\n");
        for peripheral in &rendered {
            source.push_str(&format!(
                "    pub fn take_{}(&mut self) -> Option<{}> {{ self.{}.take() }}\n",
                peripheral.field, peripheral.alias, peripheral.field
            ));
        }
        source.push_str("}\n\n");
        for peripheral in &rendered {
            match peripheral.peripheral_api.as_str() {
                "display" => source.push_str(&format!(
                    "impl ::barracuda_board_hal::display::DisplayPeripheral for GeneratedPeripherals {{\n    type Display = {};\n    fn take_display(&mut self) -> Option<Self::Display> {{ self.{}.take() }}\n}}\n\n",
                    peripheral.alias, peripheral.field
                )),
                "indicator" => source.push_str(&format!(
                    "impl ::barracuda_board_hal::indicator::IndicatorPeripheral for GeneratedPeripherals {{\n    type Indicator = {};\n    fn take_indicator(&mut self) -> Option<Self::Indicator> {{ self.{}.take() }}\n}}\n\n",
                    peripheral.alias, peripheral.field
                )),
                "led-strip" => source.push_str(&format!(
                    "impl ::barracuda_board_hal::led_strip::LedStripPeripheral for GeneratedPeripherals {{\n    type LedStrip = {};\n    fn take_led_strip(&mut self) -> Option<Self::LedStrip> {{ self.{}.take() }}\n}}\n\n",
                    peripheral.alias, peripheral.field
                )),
                "camera" => source.push_str(&format!(
                    "impl ::barracuda_board_hal::camera::CameraPeripheral for GeneratedPeripherals {{\n    type Camera = {};\n    fn take_camera(&mut self) -> Option<Self::Camera> {{ self.{}.take() }}\n}}\n\n",
                    peripheral.alias, peripheral.field
                )),
                "audio-codec" => source.push_str(&format!(
                    "impl ::barracuda_board_hal::audio::AudioCodecPeripheral for GeneratedPeripherals {{\n    type AudioCodec = {};\n    fn take_audio_codec(&mut self) -> Option<Self::AudioCodec> {{ self.{}.take() }}\n}}\n\n",
                    peripheral.alias, peripheral.field
                )),
                "inertial-measurement" => source.push_str(&format!(
                    "impl ::barracuda_board_hal::imu::ImuPeripheral for GeneratedPeripherals {{\n    type Imu = {};\n    fn take_imu(&mut self) -> Option<Self::Imu> {{ self.{}.take() }}\n}}\n\n",
                    peripheral.alias, peripheral.field
                )),
                "power-monitor" => source.push_str(&format!(
                    "impl ::barracuda_board_hal::power::PowerMonitorPeripheral for GeneratedPeripherals {{\n    type PowerMonitor = {};\n    fn take_power_monitor(&mut self) -> Option<Self::PowerMonitor> {{ self.{}.take() }}\n}}\n\n",
                    peripheral.alias, peripheral.field
                )),
                "removable-storage" => source.push_str(&format!(
                    "impl ::barracuda_board_hal::removable_storage::RemovableStoragePeripheral for GeneratedPeripherals {{\n    type RemovableStorage = {};\n    fn take_removable_storage(&mut self) -> Option<Self::RemovableStorage> {{ self.{}.take() }}\n}}\n\n",
                    peripheral.alias, peripheral.field
                )),
                "real-time-clock" => source.push_str(&format!(
                    "impl ::barracuda_board_hal::real_time_clock::RealTimeClockPeripheral for GeneratedPeripherals {{\n    type RealTimeClock = {};\n    fn take_real_time_clock(&mut self) -> Option<Self::RealTimeClock> {{ self.{}.take() }}\n}}\n\n",
                    peripheral.alias, peripheral.field
                )),
                "touch" => source.push_str(&format!(
                    "impl ::barracuda_board_hal::touch::TouchPeripheral for GeneratedPeripherals {{\n    type Touch = {};\n    fn take_touch(&mut self) -> Option<Self::Touch> {{ self.{}.take() }}\n}}\n\n",
                    peripheral.alias, peripheral.field
                )),
                "buttons" => source.push_str(&format!(
                    "impl ::barracuda_board_hal::buttons::ButtonsPeripheral for GeneratedPeripherals {{\n    type Buttons = {};\n    fn take_buttons(&mut self) -> Option<Self::Buttons> {{ self.{}.take() }}\n}}\n\n",
                    peripheral.alias, peripheral.field
                )),
                _ => {}
            }
        }
        if !primary_peripherals.contains("led-strip") {
            source.push_str(
                "impl ::barracuda_board_hal::led_strip::LedStripPeripheral for GeneratedPeripherals {\n    type LedStrip = ::barracuda_board_hal::UnavailableLedStrip;\n    fn take_led_strip(&mut self) -> Option<Self::LedStrip> { None }\n}\n\n",
            );
        }
        if !primary_peripherals.contains("camera") {
            source.push_str(
                "impl ::barracuda_board_hal::camera::CameraPeripheral for GeneratedPeripherals {\n    type Camera = ::barracuda_board_hal::UnavailableCamera;\n    fn take_camera(&mut self) -> Option<Self::Camera> { None }\n}\n\n",
            );
        }
        if !primary_peripherals.contains("display") {
            source.push_str(
                "impl ::barracuda_board_hal::display::DisplayPeripheral for GeneratedPeripherals {\n    type Display = ::barracuda_board_hal::UnavailableDisplay;\n    fn take_display(&mut self) -> Option<Self::Display> { None }\n}\n\n",
            );
        }
        if !primary_peripherals.contains("audio-codec") {
            source.push_str(
                "impl ::barracuda_board_hal::audio::AudioCodecPeripheral for GeneratedPeripherals {\n    type AudioCodec = ::barracuda_board_hal::UnavailableAudioCodec;\n    fn take_audio_codec(&mut self) -> Option<Self::AudioCodec> { None }\n}\n\n",
            );
        }
        if !primary_peripherals.contains("inertial-measurement") {
            source.push_str(
                "impl ::barracuda_board_hal::imu::ImuPeripheral for GeneratedPeripherals {\n    type Imu = ::barracuda_board_hal::UnavailableImu;\n    fn take_imu(&mut self) -> Option<Self::Imu> { None }\n}\n\n",
            );
        }
        if !primary_peripherals.contains("removable-storage") {
            source.push_str(
                "impl ::barracuda_board_hal::removable_storage::RemovableStoragePeripheral for GeneratedPeripherals {\n    type RemovableStorage = ::barracuda_board_hal::UnavailableRemovableStorage;\n    fn take_removable_storage(&mut self) -> Option<Self::RemovableStorage> { None }\n}\n\n",
            );
        }
        if !primary_peripherals.contains("power-monitor") {
            source.push_str(
                "impl ::barracuda_board_hal::power::PowerMonitorPeripheral for GeneratedPeripherals {\n    type PowerMonitor = ::barracuda_board_hal::UnavailablePowerMonitor;\n    fn take_power_monitor(&mut self) -> Option<Self::PowerMonitor> { None }\n}\n\n",
            );
        }
        if !primary_peripherals.contains("real-time-clock") {
            source.push_str(
                "impl ::barracuda_board_hal::real_time_clock::RealTimeClockPeripheral for GeneratedPeripherals {\n    type RealTimeClock = ::barracuda_board_hal::UnavailableRealTimeClock;\n    fn take_real_time_clock(&mut self) -> Option<Self::RealTimeClock> { None }\n}\n\n",
            );
        }
        if !primary_peripherals.contains("touch") {
            source.push_str(
                "impl ::barracuda_board_hal::touch::TouchPeripheral for GeneratedPeripherals {\n    type Touch = ::barracuda_board_hal::UnavailableTouch;\n    fn take_touch(&mut self) -> Option<Self::Touch> { None }\n}\n\n",
            );
        }
        if !primary_peripherals.contains("buttons") {
            source.push_str(
                "impl ::barracuda_board_hal::buttons::ButtonsPeripheral for GeneratedPeripherals {\n    type Buttons = ::barracuda_board_hal::UnavailableButtons;\n    fn take_buttons(&mut self) -> Option<Self::Buttons> { None }\n}\n\n",
            );
        }
    }

    source.push_str(&format!(
        "pub type GeneratedExposedIo = ::barracuda_platform_selected::__platform::hal::RuntimeIo<{pin_count}, {}, {}, {}, {}, {}, {}>;\n\n",
        runtime_i2c_controllers.len(),
        runtime_spi_controllers.len(),
        runtime_uart_controllers.len(),
        runtime_adc_controllers.len(),
        runtime_pwm_controllers.len(),
        runtime_i2s_controllers.len(),
    ));

    if rendered.is_empty() && state.binding_errors.is_empty() {
        source.push_str("type GeneratedBoardError = core::convert::Infallible;\n\n");
    } else {
        source.push_str(
            "#[derive(Debug)]\n#[allow(clippy::enum_variant_names)]\npub enum GeneratedBoardError {\n",
        );
        for (variant, ty) in &state.binding_errors {
            source.push_str(&format!("    {variant}({ty}),\n"));
        }
        for peripheral in &rendered {
            source.push_str(&format!(
                "    {}(<{}Factory as PeripheralImplementation>::Error),\n",
                peripheral.error_variant, peripheral.error_variant
            ));
        }
        source.push_str("}\n\nimpl fmt::Display for GeneratedBoardError {\n    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {\n        match self {\n");
        for (variant, _) in &state.binding_errors {
            source.push_str(&format!(
                "            Self::{variant}(error) => write!(formatter, {:?}, error),\n",
                format!("failed to construct `{}` binding", variant) + ": {:?}"
            ));
        }
        for peripheral in &rendered {
            source.push_str(&format!(
                "            Self::{}(error) => write!(formatter, {:?}, error),\n",
                peripheral.error_variant,
                format!("failed to initialize `{}`: {{:?}}", peripheral.name)
            ));
        }
        source.push_str(
            "        }\n    }\n}\n\nimpl core::error::Error for GeneratedBoardError {}\n\n",
        );
    }

    source.push_str(
        "pub struct SelectedBoardHal;\n\nimpl BoardHal for SelectedBoardHal {\n    type Bindings = GeneratedBoardBindings;\n    type Resources = BoardResources<GeneratedPeripherals, GeneratedExposedIo>;\n    type Error = GeneratedBoardError;\n\n    async fn initialize(_spawner: Spawner, bindings: Self::Bindings) -> BoardHalInitResult<Self> {\n",
    );
    for bus in state.i2c_buses.values() {
        source.push_str(&format!(
            "        let {}: &'static ::barracuda_platform_selected::__platform::hal::I2cBusManager = ::barracuda_platform_selected::__platform::hal::i2c_bus_manager!(::barracuda_platform_selected::__platform::hal::i2c_device(bindings.{}, bindings.{}, bindings.{}, {}).map_err(GeneratedBoardError::{})?);\n",
            bus.local,
            bus.controller,
            bus.scl,
            bus.sda,
            bus.frequency_hz,
            bus.error_variant,
        ));
    }
    if peripheral_models {
        // The HAL attaches each declared peripheral's device model before
        // any implementation starts talking to its bus.
        for peripheral in &rendered {
            source.push_str(&format!(
                "        ::barracuda_platform_selected::__platform::hal::declare_peripheral(&{});\n",
                peripheral.declaration
            ));
        }
    }
    for peripheral in &rendered {
        source.push_str(&format!(
            "        let {} = <{}Factory as PeripheralImplementation>::initialize({}, {}).await.map_err(GeneratedBoardError::{})?;\n",
            peripheral.field,
            peripheral.error_variant,
            peripheral.bindings,
            peripheral.config,
            peripheral.error_variant,
        ));
    }
    if rendered.is_empty() {
        source.push_str("        let peripherals = ::barracuda_board_hal::NoPeripherals;\n");
    } else {
        source.push_str("        let peripherals = GeneratedPeripherals {\n");
        for peripheral in &rendered {
            source.push_str(&format!(
                "            {}: Some({}),\n",
                peripheral.field, peripheral.field
            ));
        }
        source.push_str("        };\n");
    }
    source.push_str(
        "        let exposed_io = ::barracuda_platform_selected::__platform::hal::runtime_io([\n",
    );
    for (name, _) in board.exposed_io().pins() {
        let field = rust_identifier(&format!("pin_{name}"));
        source.push_str(&format!(
            "            ({name:?}, ::barracuda_platform_selected::__platform::hal::runtime_pin(bindings.{field})),\n"
        ));
    }
    source.push_str("        ], [\n");
    for controller in &runtime_i2c_controllers {
        let field = rust_identifier(&format!("runtime_controller_{controller}"));
        source.push_str(&format!(
            "            ::barracuda_platform_selected::__platform::hal::runtime_i2c_controller(bindings.{field}),\n"
        ));
    }
    source.push_str("        ], [\n");
    for controller in &runtime_spi_controllers {
        let field = rust_identifier(&format!("runtime_controller_{controller}"));
        source.push_str(&format!(
            "            ::barracuda_platform_selected::__platform::hal::runtime_spi_controller(bindings.{field}),\n"
        ));
    }
    source.push_str("        ], [\n");
    for controller in &runtime_uart_controllers {
        let field = rust_identifier(&format!("runtime_controller_{controller}"));
        source.push_str(&format!(
            "            ::barracuda_platform_selected::__platform::hal::runtime_uart_controller(bindings.{field}),\n"
        ));
    }
    source.push_str("        ], [\n");
    for resource in &runtime_adc_controllers {
        let field = rust_identifier(&format!("runtime_adc_controller_{}", resource.controller()));
        source.push_str(&format!(
            "            ::barracuda_platform_selected::__platform::hal::runtime_adc_resource(bindings.{field}, &[\n"
        ));
        for channel in resource.channels() {
            source.push_str(&format!(
                "                ::barracuda_platform_selected::__platform::hal::runtime_adc_channel!({}, {}),\n",
                channel.channel(),
                channel.pin(),
            ));
        }
        source.push_str("            ]),\n");
    }
    source.push_str("        ], [\n");
    for resource in &runtime_pwm_controllers {
        let field = rust_identifier(&format!("runtime_pwm_controller_{}", resource.controller()));
        source.push_str(&format!(
            "            ::barracuda_platform_selected::__platform::hal::runtime_pwm_resource(bindings.{field}, &[\n"
        ));
        for timer in resource.timers() {
            source.push_str(&format!(
                "                ::barracuda_platform_selected::__platform::hal::runtime_pwm_timer!({timer}),\n"
            ));
        }
        source.push_str("            ], &[\n");
        for channel in resource.channels() {
            source.push_str(&format!(
                "                ::barracuda_platform_selected::__platform::hal::runtime_pwm_channel!({channel}),\n"
            ));
        }
        source.push_str("            ]),\n");
    }
    source.push_str("        ], [\n");
    for (index, resource) in runtime_i2s_controllers.iter().enumerate() {
        let controller_field =
            rust_identifier(&format!("runtime_i2s_controller_{}", resource.controller()));
        source.push_str(&format!(
            "            ::barracuda_platform_selected::__platform::hal::runtime_i2s_resource(bindings.{controller_field}, [\n"
        ));
        for dma in resource.dma_channels() {
            let field = rust_identifier(&format!("runtime_i2s_dma_{dma}"));
            source.push_str(&format!(
                "                ::barracuda_platform_selected::__platform::hal::runtime_i2s_dma(bindings.{field}),\n"
            ));
        }
        source.push_str(&format!(
            "            ], ::barracuda_platform_selected::__platform::hal::runtime_i2s_dma_buffers!({}).map_err(GeneratedBoardError::RuntimeI2s{index})?),\n",
            resource.dma_buffer_bytes(),
        ));
    }
    source.push_str("        ]);\n");
    source.push_str("        Ok(BoardResources::new(peripherals, exposed_io))\n    }\n}\n}\n\n");
    source.push_str("pub use generated_board_hal::*;\n");
    Ok(source)
}

/// Renders the Board declaration of one peripheral as a
/// `hal::PeripheralDeclaration` expression for a HAL with peripheral models.
///
/// Bindings name the chip-native resource each role resolves to: the
/// controller of an I2C device and the pin of a digital binding. Other
/// binding kinds and structured parameters are omitted.
fn render_declaration(
    board: &BoardDefinition,
    peripheral: &ResolvedPeripheral<'_>,
) -> Result<String, GenerateError> {
    let mut bindings = Vec::new();
    for (role, schema) in peripheral.implementation().bindings() {
        let Some(resource) = peripheral.binding(role) else {
            continue;
        };
        let native = match schema.kind() {
            BindingKind::I2cDevice => board
                .peripheral_io()
                .i2c_device(resource)
                .map(|i2c| i2c.peripheral()),
            BindingKind::DigitalInput | BindingKind::DigitalOutput => Some(resource),
            _ => None,
        };
        if let Some(native) = native {
            validate_hardware_identifier(native)?;
            bindings.push(format!("({role:?}, {native:?})"));
        }
    }
    let mut parameters = Vec::new();
    for name in peripheral.implementation().parameters.keys() {
        let value = match peripheral.parameter(name) {
            Some(PeripheralParameter::Boolean(value)) => value.to_string(),
            Some(PeripheralParameter::Integer(value)) => value.to_string(),
            Some(PeripheralParameter::String(value)) => value.clone(),
            _ => continue,
        };
        parameters.push(format!("({name:?}, {value:?})"));
    }
    Ok(format!(
        "::barracuda_platform_selected::__platform::hal::PeripheralDeclaration {{ name: {:?}, implementation: {:?}, bindings: &[{}], parameters: &[{}] }}",
        peripheral.name(),
        peripheral.implementation().id(),
        bindings.join(", "),
        parameters.join(", "),
    ))
}

fn optional_binding_type(
    peripheral: &ResolvedPeripheral<'_>,
    role: &str,
    kind: BindingKind,
) -> Result<String, GenerateError> {
    match kind {
        BindingKind::DigitalOutput => Ok(String::from(
            "::barracuda_platform_selected::__platform::hal::DigitalOutput",
        )),
        _ => Err(GenerateError::UnsupportedBindingKind {
            implementation: peripheral.implementation().id().to_owned(),
            binding: role.to_owned(),
            kind,
        }),
    }
}

fn render_binding(
    board: &BoardDefinition,
    peripheral: &ResolvedPeripheral<'_>,
    role: &str,
    schema: &BindingSchema,
    resource: &str,
    runtime_spi_controllers: &[String],
    state: &mut RenderState,
) -> Result<(String, String), GenerateError> {
    match schema.kind() {
        BindingKind::DigitalInput | BindingKind::DigitalOutput => {
            validate_hardware_identifier(resource)?;
            let field = checked_identifier(
                &format!("{}_{}", peripheral.name(), role),
                &mut state.identifiers,
            )?;
            state.raw_fields.push((
                field.clone(),
                format!(
                    "::barracuda_platform_selected::__platform::hal::pin_binding_type!({resource})"
                ),
            ));
            if schema.kind() == BindingKind::DigitalInput {
                Ok((
                    String::from("::barracuda_platform_selected::__platform::hal::DigitalInput"),
                    format!(
                        "::barracuda_platform_selected::__platform::hal::digital_input(bindings.{field})"
                    ),
                ))
            } else {
                let initial = resolved_initial_level(peripheral, role, schema)?;
                Ok((
                    String::from("::barracuda_platform_selected::__platform::hal::DigitalOutput"),
                    format!(
                        "::barracuda_platform_selected::__platform::hal::digital_output(bindings.{field}, ::barracuda_board_hal::DigitalLevel::{initial})"
                    ),
                ))
            }
        }
        BindingKind::SpiDevice => {
            let spi = board.peripheral_io().spi_device(resource).ok_or_else(|| {
                GenerateError::UnsupportedResolvedValue {
                    peripheral: peripheral.name().to_owned(),
                    parameter: format!("{role} binding"),
                }
            })?;
            if spi.mosi().is_none() && spi.miso().is_none() {
                return Err(GenerateError::UnsupportedResolvedValue {
                    peripheral: peripheral.name().to_owned(),
                    parameter: format!("{role}.mosi-or-miso"),
                });
            }
            for identifier in [
                Some(spi.peripheral()),
                Some(spi.sck()),
                spi.mosi(),
                spi.miso(),
                Some(spi.chip_select()),
            ]
            .into_iter()
            .flatten()
            {
                validate_hardware_identifier(identifier)?;
            }
            let prefix = format!("{}_{}", peripheral.name(), role);
            let controller =
                checked_identifier(&format!("{prefix}_controller"), &mut state.identifiers)?;
            let sck = checked_identifier(&format!("{prefix}_sck"), &mut state.identifiers)?;
            let chip_select =
                checked_identifier(&format!("{prefix}_chip_select"), &mut state.identifiers)?;
            state.raw_fields.extend([
                (
                    controller.clone(),
                    format!(
                        "::barracuda_platform_selected::__platform::hal::controller_binding_type!({})",
                        spi.peripheral()
                    ),
                ),
                (
                    sck.clone(),
                    format!("::barracuda_platform_selected::__platform::hal::pin_binding_type!({})", spi.sck()),
                ),
                (
                    chip_select.clone(),
                    format!(
                        "::barracuda_platform_selected::__platform::hal::pin_binding_type!({})",
                        spi.chip_select()
                    ),
                ),
            ]);
            let mosi_field = spi
                .mosi()
                .map(|mosi| {
                    let field =
                        checked_identifier(&format!("{prefix}_mosi"), &mut state.identifiers)?;
                    state.raw_fields.push((
                        field.clone(),
                        format!("::barracuda_platform_selected::__platform::hal::pin_binding_type!({mosi})"),
                    ));
                    Ok(field)
                })
                .transpose()?;
            let miso_field = spi
                .miso()
                .map(|miso| {
                    let field =
                        checked_identifier(&format!("{prefix}_miso"), &mut state.identifiers)?;
                    state.raw_fields.push((
                        field.clone(),
                        format!("::barracuda_platform_selected::__platform::hal::pin_binding_type!({miso})"),
                    ));
                    Ok(field)
                })
                .transpose()?;
            let variant = format!(
                "{}{}Binding",
                pascal_identifier(peripheral.name()),
                pascal_identifier(role)
            );
            state.binding_errors.push((
                variant.clone(),
                String::from("::barracuda_platform_selected::__platform::hal::SpiConfigError"),
            ));
            let value = match (mosi_field, miso_field) {
                (Some(mosi), Some(miso)) => format!(
                    "::barracuda_platform_selected::__platform::hal::spi_device_full_duplex(bindings.{controller}, bindings.{sck}, bindings.{mosi}, bindings.{miso}, bindings.{chip_select}, {}).map_err(GeneratedBoardError::{variant})?",
                    spi.frequency_hz()
                ),
                (Some(mosi), None) => format!(
                    "::barracuda_platform_selected::__platform::hal::spi_device(bindings.{controller}, bindings.{sck}, bindings.{mosi}, bindings.{chip_select}, {}).map_err(GeneratedBoardError::{variant})?",
                    spi.frequency_hz()
                ),
                (None, Some(miso)) => format!(
                    "::barracuda_platform_selected::__platform::hal::spi_device_rx_only(bindings.{controller}, bindings.{sck}, bindings.{miso}, bindings.{chip_select}, {}).map_err(GeneratedBoardError::{variant})?",
                    spi.frequency_hz()
                ),
                (None, None) => {
                    return Err(GenerateError::UnsupportedResolvedValue {
                        peripheral: peripheral.name().to_owned(),
                        parameter: format!("{role}.mosi-or-miso"),
                    });
                }
            };
            Ok((
                String::from("::barracuda_platform_selected::__platform::hal::SpiDevice"),
                value,
            ))
        }
        BindingKind::QuadSpiDevice => {
            let spi = board
                .peripheral_io()
                .quad_spi_device(resource)
                .ok_or_else(|| GenerateError::UnsupportedResolvedValue {
                    peripheral: peripheral.name().to_owned(),
                    parameter: format!("{role} binding"),
                })?;
            for identifier in [spi.peripheral(), spi.sck(), spi.chip_select()]
                .into_iter()
                .chain(spi.data().iter().map(String::as_str))
            {
                validate_hardware_identifier(identifier)?;
            }
            let prefix = format!("{}_{}", peripheral.name(), role);
            let controller =
                checked_identifier(&format!("{prefix}_controller"), &mut state.identifiers)?;
            let sck = checked_identifier(&format!("{prefix}_sck"), &mut state.identifiers)?;
            let chip_select =
                checked_identifier(&format!("{prefix}_chip_select"), &mut state.identifiers)?;
            let mut data_fields = Vec::new();
            state.raw_fields.extend([
                (
                    controller.clone(),
                    format!(
                        "::barracuda_platform_selected::__platform::hal::controller_binding_type!({})",
                        spi.peripheral()
                    ),
                ),
                (
                    sck.clone(),
                    format!(
                        "::barracuda_platform_selected::__platform::hal::pin_binding_type!({})",
                        spi.sck()
                    ),
                ),
                (
                    chip_select.clone(),
                    format!(
                        "::barracuda_platform_selected::__platform::hal::pin_binding_type!({})",
                        spi.chip_select()
                    ),
                ),
            ]);
            for (index, pin) in spi.data().iter().enumerate() {
                let field =
                    checked_identifier(&format!("{prefix}_d{index}"), &mut state.identifiers)?;
                state.raw_fields.push((
                    field.clone(),
                    format!(
                        "::barracuda_platform_selected::__platform::hal::pin_binding_type!({pin})"
                    ),
                ));
                data_fields.push(field);
            }
            let variant = format!(
                "{}{}Binding",
                pascal_identifier(peripheral.name()),
                pascal_identifier(role)
            );
            state.binding_errors.push((
                variant.clone(),
                String::from("::barracuda_platform_selected::__platform::hal::QuadSpiConfigError"),
            ));
            Ok((
                String::from("::barracuda_platform_selected::__platform::hal::QuadSpiDevice"),
                format!(
                    "::barracuda_platform_selected::__platform::hal::quad_spi_device(bindings.{controller}, bindings.{sck}, bindings.{}, bindings.{}, bindings.{}, bindings.{}, bindings.{chip_select}, {}).map_err(GeneratedBoardError::{variant})?",
                    data_fields[0],
                    data_fields[1],
                    data_fields[2],
                    data_fields[3],
                    spi.frequency_hz(),
                ),
            ))
        }
        BindingKind::SpiBus => {
            let spi = board.peripheral_io().spi_bus(resource).ok_or_else(|| {
                GenerateError::UnsupportedResolvedValue {
                    peripheral: peripheral.name().to_owned(),
                    parameter: format!("{role} binding"),
                }
            })?;
            for identifier in [
                Some(spi.peripheral()),
                Some(spi.sck()),
                spi.mosi(),
                spi.miso(),
            ]
            .into_iter()
            .flatten()
            {
                validate_hardware_identifier(identifier)?;
            }
            let prefix = format!("{}_{}", peripheral.name(), role);
            let controller =
                checked_identifier(&format!("{prefix}_controller"), &mut state.identifiers)?;
            let sck = checked_identifier(&format!("{prefix}_sck"), &mut state.identifiers)?;
            state.raw_fields.extend([
                (
                    controller.clone(),
                    format!("::barracuda_platform_selected::__platform::hal::controller_binding_type!({})", spi.peripheral()),
                ),
                (
                    sck.clone(),
                    format!("::barracuda_platform_selected::__platform::hal::pin_binding_type!({})", spi.sck()),
                ),
            ]);
            let mosi = spi.mosi().map(|pin| {
                let field = checked_identifier(&format!("{prefix}_mosi"), &mut state.identifiers)?;
                state.raw_fields.push((field.clone(), format!("::barracuda_platform_selected::__platform::hal::pin_binding_type!({pin})")));
                Ok(field)
            }).transpose()?;
            let miso = spi.miso().map(|pin| {
                let field = checked_identifier(&format!("{prefix}_miso"), &mut state.identifiers)?;
                state.raw_fields.push((field.clone(), format!("::barracuda_platform_selected::__platform::hal::pin_binding_type!({pin})")));
                Ok(field)
            }).transpose()?;
            let variant = format!(
                "{}{}Binding",
                pascal_identifier(peripheral.name()),
                pascal_identifier(role)
            );
            state.binding_errors.push((
                variant.clone(),
                String::from("::barracuda_platform_selected::__platform::hal::SpiConfigError"),
            ));
            let value = match (mosi, miso) {
                (Some(mosi), Some(miso)) => format!(
                    "::barracuda_platform_selected::__platform::hal::spi_bus_full_duplex(bindings.{controller}, bindings.{sck}, bindings.{mosi}, bindings.{miso}, {}).map_err(GeneratedBoardError::{variant})?",
                    spi.frequency_hz()
                ),
                (Some(mosi), None) => format!(
                    "::barracuda_platform_selected::__platform::hal::spi_bus(bindings.{controller}, bindings.{sck}, bindings.{mosi}, {}).map_err(GeneratedBoardError::{variant})?",
                    spi.frequency_hz()
                ),
                (None, Some(miso)) => format!(
                    "::barracuda_platform_selected::__platform::hal::spi_bus_rx_only(bindings.{controller}, bindings.{sck}, bindings.{miso}, {}).map_err(GeneratedBoardError::{variant})?",
                    spi.frequency_hz()
                ),
                (None, None) => {
                    return Err(GenerateError::UnsupportedResolvedValue {
                        peripheral: peripheral.name().to_owned(),
                        parameter: format!("{role}.mosi-or-miso"),
                    });
                }
            };
            Ok((
                String::from("::barracuda_platform_selected::__platform::hal::SpiBus"),
                value,
            ))
        }
        BindingKind::SpiOutput => {
            let output = board.peripheral_io().spi_output(resource).ok_or_else(|| {
                GenerateError::UnsupportedResolvedValue {
                    peripheral: peripheral.name().to_owned(),
                    parameter: format!("{role} binding"),
                }
            })?;
            validate_hardware_identifier(output.data())?;
            let controller = runtime_spi_controllers
                .iter()
                .find(|controller| {
                    !board.peripheral_io().uses_controller(controller)
                        && !state.reserved_spi_controllers.contains(*controller)
                })
                .ok_or(GenerateError::MissingPlatformSpiController)?;
            state.reserved_spi_controllers.insert(controller.clone());

            let prefix = format!("{}_{}", peripheral.name(), role);
            let controller_field =
                checked_identifier(&format!("{prefix}_controller"), &mut state.identifiers)?;
            let data_field = checked_identifier(&format!("{prefix}_data"), &mut state.identifiers)?;
            state.raw_fields.extend([
                (
                    controller_field.clone(),
                    format!("::barracuda_platform_selected::__platform::hal::controller_binding_type!({controller})"),
                ),
                (
                    data_field.clone(),
                    format!("::barracuda_platform_selected::__platform::hal::pin_binding_type!({})", output.data()),
                ),
            ]);
            let variant = format!(
                "{}{}Binding",
                pascal_identifier(peripheral.name()),
                pascal_identifier(role)
            );
            state.binding_errors.push((
                variant.clone(),
                String::from("::barracuda_platform_selected::__platform::hal::SpiConfigError"),
            ));
            Ok((
                String::from("::barracuda_platform_selected::__platform::hal::SpiBus"),
                format!(
                    "::barracuda_platform_selected::__platform::hal::spi_output(bindings.{controller_field}, bindings.{data_field}, {}).map_err(GeneratedBoardError::{variant})?",
                    output.frequency_hz()
                ),
            ))
        }
        BindingKind::I2cDevice => {
            let i2c = board.peripheral_io().i2c_device(resource).ok_or_else(|| {
                GenerateError::UnsupportedResolvedValue {
                    peripheral: peripheral.name().to_owned(),
                    parameter: format!("{role} binding"),
                }
            })?;
            for identifier in [i2c.peripheral(), i2c.scl(), i2c.sda()] {
                validate_hardware_identifier(identifier)?;
            }
            if !state.i2c_buses.contains_key(resource) {
                let prefix = format!("shared_i2c_{resource}");
                let local = checked_identifier(&prefix, &mut state.identifiers)?;
                let controller =
                    checked_identifier(&format!("{prefix}_controller"), &mut state.identifiers)?;
                let scl = checked_identifier(&format!("{prefix}_scl"), &mut state.identifiers)?;
                let sda = checked_identifier(&format!("{prefix}_sda"), &mut state.identifiers)?;
                state.raw_fields.extend([
                    (
                        controller.clone(),
                        format!(
                            "::barracuda_platform_selected::__platform::hal::controller_binding_type!({})",
                            i2c.peripheral()
                        ),
                    ),
                    (
                        scl.clone(),
                        format!("::barracuda_platform_selected::__platform::hal::pin_binding_type!({})", i2c.scl()),
                    ),
                    (
                        sda.clone(),
                        format!("::barracuda_platform_selected::__platform::hal::pin_binding_type!({})", i2c.sda()),
                    ),
                ]);
                let error_variant = format!("{}Binding", pascal_identifier(&prefix));
                state.binding_errors.push((
                    error_variant.clone(),
                    String::from("::barracuda_platform_selected::__platform::hal::I2cConfigError"),
                ));
                state.i2c_buses.insert(
                    resource.to_owned(),
                    RenderedI2cBus {
                        local,
                        controller,
                        scl,
                        sda,
                        frequency_hz: i2c.frequency_hz(),
                        error_variant,
                    },
                );
            }
            let bus = state.i2c_buses.get(resource).ok_or_else(|| {
                GenerateError::UnsupportedResolvedValue {
                    peripheral: peripheral.name().to_owned(),
                    parameter: format!("{role} binding"),
                }
            })?;
            Ok((
                String::from("::barracuda_platform_selected::__platform::hal::I2cDevice"),
                format!("{}.device()", bus.local),
            ))
        }
        BindingKind::DsiHost => {
            let host = board.peripheral_io().dsi_host(resource).ok_or_else(|| {
                GenerateError::UnsupportedResolvedValue {
                    peripheral: peripheral.name().to_owned(),
                    parameter: format!("{role} binding"),
                }
            })?;
            validate_hardware_identifier(host.peripheral())?;
            validate_hardware_identifier(host.dma())?;
            validate_hardware_identifier(host.backlight())?;
            let prefix = format!("{}_{}", peripheral.name(), role);
            let controller =
                checked_identifier(&format!("{prefix}_controller"), &mut state.identifiers)?;
            let dma = checked_identifier(&format!("{prefix}_dma"), &mut state.identifiers)?;
            let backlight =
                checked_identifier(&format!("{prefix}_backlight"), &mut state.identifiers)?;
            state.raw_fields.extend([
                (
                    controller.clone(),
                    format!(
                        "::barracuda_platform_selected::__platform::hal::controller_binding_type!({})",
                        host.peripheral()
                    ),
                ),
                (
                    dma.clone(),
                    format!(
                        "::barracuda_platform_selected::__platform::hal::controller_binding_type!({})",
                        host.dma()
                    ),
                ),
                (
                    backlight.clone(),
                    format!(
                        "::barracuda_platform_selected::__platform::hal::pin_binding_type!({})",
                        host.backlight()
                    ),
                ),
            ]);
            let variant = format!(
                "{}{}Binding",
                pascal_identifier(peripheral.name()),
                pascal_identifier(role)
            );
            state.binding_errors.push((
                variant.clone(),
                String::from("::barracuda_platform_selected::__platform::hal::DsiConfigError"),
            ));
            Ok((
                String::from("::barracuda_platform_selected::__platform::hal::DsiHost"),
                format!(
                    "::barracuda_platform_selected::__platform::hal::dsi_host(bindings.{controller}, bindings.{dma}, bindings.{backlight}, {}, {}, {}).map_err(GeneratedBoardError::{variant})?",
                    host.data_lanes(),
                    host.phy_power_channel(),
                    host.phy_power_millivolts(),
                ),
            ))
        }
        BindingKind::MipiCsi => {
            let host = board.peripheral_io().mipi_csi(resource).ok_or_else(|| {
                GenerateError::UnsupportedResolvedValue {
                    peripheral: peripheral.name().to_owned(),
                    parameter: format!("{role} binding"),
                }
            })?;
            validate_hardware_identifier(host.peripheral())?;
            let i2c_resource = host.i2c();
            let i2c = board
                .peripheral_io()
                .i2c_device(i2c_resource)
                .ok_or_else(|| GenerateError::UnsupportedResolvedValue {
                    peripheral: peripheral.name().to_owned(),
                    parameter: format!("{role} I2C binding"),
                })?;
            for identifier in [i2c.peripheral(), i2c.scl(), i2c.sda()] {
                validate_hardware_identifier(identifier)?;
            }
            if !state.i2c_buses.contains_key(i2c_resource) {
                let prefix = format!("shared_i2c_{i2c_resource}");
                let local = checked_identifier(&prefix, &mut state.identifiers)?;
                let controller =
                    checked_identifier(&format!("{prefix}_controller"), &mut state.identifiers)?;
                let scl = checked_identifier(&format!("{prefix}_scl"), &mut state.identifiers)?;
                let sda = checked_identifier(&format!("{prefix}_sda"), &mut state.identifiers)?;
                state.raw_fields.extend([
                    (
                        controller.clone(),
                        format!(
                            "::barracuda_platform_selected::__platform::hal::controller_binding_type!({})",
                            i2c.peripheral()
                        ),
                    ),
                    (
                        scl.clone(),
                        format!("::barracuda_platform_selected::__platform::hal::pin_binding_type!({})", i2c.scl()),
                    ),
                    (
                        sda.clone(),
                        format!("::barracuda_platform_selected::__platform::hal::pin_binding_type!({})", i2c.sda()),
                    ),
                ]);
                let error_variant = format!("{}Binding", pascal_identifier(&prefix));
                state.binding_errors.push((
                    error_variant.clone(),
                    String::from("::barracuda_platform_selected::__platform::hal::I2cConfigError"),
                ));
                state.i2c_buses.insert(
                    i2c_resource.to_owned(),
                    RenderedI2cBus {
                        local,
                        controller,
                        scl,
                        sda,
                        frequency_hz: i2c.frequency_hz(),
                        error_variant,
                    },
                );
            }
            let bus_local = state
                .i2c_buses
                .get(i2c_resource)
                .ok_or_else(|| GenerateError::UnsupportedResolvedValue {
                    peripheral: peripheral.name().to_owned(),
                    parameter: format!("{role} I2C binding"),
                })?
                .local
                .clone();
            let prefix = format!("{}_{}", peripheral.name(), role);
            let controller =
                checked_identifier(&format!("{prefix}_controller"), &mut state.identifiers)?;
            state.raw_fields.push((
                controller.clone(),
                format!(
                    "::barracuda_platform_selected::__platform::hal::controller_binding_type!({})",
                    host.peripheral()
                ),
            ));
            let variant = format!(
                "{}{}Binding",
                pascal_identifier(peripheral.name()),
                pascal_identifier(role)
            );
            state.binding_errors.push((
                variant.clone(),
                String::from("::barracuda_platform_selected::__platform::hal::MipiCsiConfigError"),
            ));
            Ok((
                String::from("::barracuda_platform_selected::__platform::hal::MipiCsiHost"),
                format!(
                    "::barracuda_platform_selected::__platform::hal::mipi_csi_host(bindings.{controller}, {}).map_err(GeneratedBoardError::{variant})?",
                    bus_local,
                ),
            ))
        }
        BindingKind::SdmmcDevice => {
            let device = board
                .peripheral_io()
                .sdmmc_device(resource)
                .ok_or_else(|| GenerateError::UnsupportedResolvedValue {
                    peripheral: peripheral.name().to_owned(),
                    parameter: format!("{role} binding"),
                })?;
            validate_hardware_identifier(device.peripheral())?;
            let prefix = format!("{}_{}", peripheral.name(), role);
            let controller =
                checked_identifier(&format!("{prefix}_controller"), &mut state.identifiers)?;
            state.raw_fields.push((
                controller.clone(),
                format!(
                    "::barracuda_platform_selected::__platform::hal::controller_binding_type!({})",
                    device.peripheral()
                ),
            ));
            let mut pin_fields = Vec::new();
            for (name, pin) in [("clk", device.clk()), ("cmd", device.cmd())]
                .into_iter()
                .chain(device.data().iter().enumerate().map(|(index, pin)| {
                    (
                        match index {
                            0 => "d0",
                            1 => "d1",
                            2 => "d2",
                            _ => "d3",
                        },
                        pin.as_str(),
                    )
                }))
            {
                validate_hardware_identifier(pin)?;
                let field =
                    checked_identifier(&format!("{prefix}_{name}"), &mut state.identifiers)?;
                state.raw_fields.push((
                    field.clone(),
                    format!(
                        "::barracuda_platform_selected::__platform::hal::pin_binding_type!({pin})"
                    ),
                ));
                pin_fields.push(field);
            }
            let variant = format!(
                "{}{}Binding",
                pascal_identifier(peripheral.name()),
                pascal_identifier(role)
            );
            state.binding_errors.push((
                variant.clone(),
                String::from("::barracuda_platform_selected::__platform::hal::SdmmcConfigError"),
            ));
            let value = match device.bus_width() {
                1 => format!(
                    "::barracuda_platform_selected::__platform::hal::sdmmc_device_1bit(bindings.{controller}, bindings.{}, bindings.{}, bindings.{}).map_err(GeneratedBoardError::{variant})?",
                    pin_fields[0], pin_fields[1], pin_fields[2],
                ),
                4 => {
                    let power_channel = device.power_channel().ok_or_else(|| {
                        GenerateError::UnsupportedResolvedValue {
                            peripheral: peripheral.name().to_owned(),
                            parameter: format!("{role}.power-channel"),
                        }
                    })?;
                    let power_millivolts = device.power_millivolts().ok_or_else(|| {
                        GenerateError::UnsupportedResolvedValue {
                            peripheral: peripheral.name().to_owned(),
                            parameter: format!("{role}.power-millivolts"),
                        }
                    })?;
                    format!(
                        "::barracuda_platform_selected::__platform::hal::sdmmc_device(bindings.{controller}, bindings.{}, bindings.{}, bindings.{}, bindings.{}, bindings.{}, bindings.{}, 4, {power_channel}, {power_millivolts}).map_err(GeneratedBoardError::{variant})?",
                        pin_fields[0],
                        pin_fields[1],
                        pin_fields[2],
                        pin_fields[3],
                        pin_fields[4],
                        pin_fields[5],
                    )
                }
                _ => {
                    return Err(GenerateError::UnsupportedResolvedValue {
                        peripheral: peripheral.name().to_owned(),
                        parameter: format!("{role}.bus-width"),
                    });
                }
            };
            Ok((
                String::from("::barracuda_platform_selected::__platform::hal::SdmmcDevice"),
                value,
            ))
        }
        BindingKind::CameraCapture => {
            let camera = board
                .peripheral_io()
                .camera_capture(resource)
                .ok_or_else(|| GenerateError::UnsupportedResolvedValue {
                    peripheral: peripheral.name().to_owned(),
                    parameter: format!("{role} binding"),
                })?;
            let prefix = format!("{}_{}", peripheral.name(), role);
            let controller =
                checked_identifier(&format!("{prefix}_controller"), &mut state.identifiers)?;
            let dma = checked_identifier(&format!("{prefix}_dma"), &mut state.identifiers)?;
            state.raw_fields.push((
                controller.clone(),
                format!(
                    "::barracuda_platform_selected::__platform::hal::controller_binding_type!({})",
                    camera.peripheral()
                ),
            ));
            state.raw_fields.push((
                dma.clone(),
                format!(
                    "::barracuda_platform_selected::__platform::hal::controller_binding_type!({})",
                    camera.dma()
                ),
            ));
            let mut pins = Vec::new();
            for (name, pin) in [
                ("xclk", camera.xclk()),
                ("pclk", camera.pclk()),
                ("vsync", camera.vsync()),
                ("href", camera.href()),
            ] {
                validate_hardware_identifier(pin)?;
                let field =
                    checked_identifier(&format!("{prefix}_{name}"), &mut state.identifiers)?;
                state.raw_fields.push((
                    field.clone(),
                    format!(
                        "::barracuda_platform_selected::__platform::hal::pin_binding_type!({pin})"
                    ),
                ));
                pins.push(field);
            }
            for (index, pin) in camera.data().iter().enumerate() {
                validate_hardware_identifier(pin)?;
                let field =
                    checked_identifier(&format!("{prefix}_d{index}"), &mut state.identifiers)?;
                state.raw_fields.push((
                    field.clone(),
                    format!(
                        "::barracuda_platform_selected::__platform::hal::pin_binding_type!({pin})"
                    ),
                ));
                pins.push(field);
            }
            let variant = format!(
                "{}{}Binding",
                pascal_identifier(peripheral.name()),
                pascal_identifier(role)
            );
            state.binding_errors.push((
                variant.clone(),
                String::from("::barracuda_platform_selected::__platform::hal::CameraConfigError"),
            ));
            let pin_values = pins
                .iter()
                .map(|field| format!("bindings.{field}"))
                .collect::<Vec<_>>()
                .join(", ");
            Ok((
                String::from("::barracuda_platform_selected::__platform::hal::CameraReceiver"),
                format!(
                    "::barracuda_platform_selected::__platform::hal::camera_capture(bindings.{controller}, bindings.{dma}, {pin_values}, {}, ::barracuda_platform_selected::__platform::hal::camera_dma_buffer!({}).map_err(GeneratedBoardError::{variant})?).map_err(GeneratedBoardError::{variant})?",
                    camera.xclk_frequency_hz(),
                    camera.dma_buffer_bytes()
                ),
            ))
        }
        BindingKind::I2sStream => {
            let stream = board.peripheral_io().i2s_stream(resource).ok_or_else(|| {
                GenerateError::UnsupportedResolvedValue {
                    peripheral: peripheral.name().to_owned(),
                    parameter: format!("{role} binding"),
                }
            })?;
            let prefix = format!("{}_{}", peripheral.name(), role);
            let controller =
                checked_identifier(&format!("{prefix}_controller"), &mut state.identifiers)?;
            let dma = checked_identifier(&format!("{prefix}_dma"), &mut state.identifiers)?;
            state.raw_fields.push((
                controller.clone(),
                format!(
                    "::barracuda_platform_selected::__platform::hal::controller_binding_type!({})",
                    stream.peripheral()
                ),
            ));
            state.raw_fields.push((
                dma.clone(),
                format!(
                    "::barracuda_platform_selected::__platform::hal::controller_binding_type!({})",
                    stream.dma()
                ),
            ));
            let mut pin_fields = Vec::new();
            for (name, pin) in [
                ("bclk", stream.bclk()),
                ("ws", stream.ws()),
                ("dout", stream.dout()),
                ("din", stream.din()),
            ] {
                validate_hardware_identifier(pin)?;
                let field =
                    checked_identifier(&format!("{prefix}_{name}"), &mut state.identifiers)?;
                state.raw_fields.push((
                    field.clone(),
                    format!(
                        "::barracuda_platform_selected::__platform::hal::pin_binding_type!({pin})"
                    ),
                ));
                pin_fields.push(field);
            }
            let mclk = stream.mclk().map(|pin| {
                validate_hardware_identifier(pin)?;
                let field = checked_identifier(&format!("{prefix}_mclk"), &mut state.identifiers)?;
                state.raw_fields.push((field.clone(), format!("::barracuda_platform_selected::__platform::hal::pin_binding_type!({pin})")));
                Ok(field)
            }).transpose()?;
            let variant = format!(
                "{}{}Binding",
                pascal_identifier(peripheral.name()),
                pascal_identifier(role)
            );
            state.binding_errors.push((
                variant.clone(),
                String::from("::barracuda_platform_selected::__platform::hal::I2sConfigError"),
            ));
            let bclk = &pin_fields[0];
            let ws = &pin_fields[1];
            let dout = &pin_fields[2];
            let din = &pin_fields[3];
            let constructor = match mclk {
                Some(mclk) => format!(
                    "{{ let (tx_dma, rx_dma) = ::barracuda_platform_selected::__platform::hal::i2s_dma_buffers!({}).map_err(GeneratedBoardError::{variant})?; ::barracuda_platform_selected::__platform::hal::i2s_stream_with_mclk(bindings.{controller}, bindings.{dma}, bindings.{bclk}, bindings.{ws}, bindings.{dout}, bindings.{din}, bindings.{mclk}, {}, {}, {}, tx_dma, rx_dma) }}",
                    stream.dma_buffer_bytes(),
                    stream.sample_rate_hz(),
                    stream.channels(),
                    stream.bits_per_sample()
                ),
                None => format!(
                    "{{ let (tx_dma, rx_dma) = ::barracuda_platform_selected::__platform::hal::i2s_dma_buffers!({}).map_err(GeneratedBoardError::{variant})?; ::barracuda_platform_selected::__platform::hal::i2s_stream(bindings.{controller}, bindings.{dma}, bindings.{bclk}, bindings.{ws}, bindings.{dout}, bindings.{din}, {}, {}, {}, tx_dma, rx_dma) }}",
                    stream.dma_buffer_bytes(),
                    stream.sample_rate_hz(),
                    stream.channels(),
                    stream.bits_per_sample()
                ),
            };
            Ok((
                String::from("::barracuda_platform_selected::__platform::hal::I2sDevice"),
                format!("{constructor}.map_err(GeneratedBoardError::{variant})?"),
            ))
        }
        BindingKind::CapacitiveTouch => {
            let touch = board
                .peripheral_io()
                .capacitive_touch(resource)
                .ok_or_else(|| GenerateError::UnsupportedResolvedValue {
                    peripheral: peripheral.name().to_owned(),
                    parameter: format!("{role} binding"),
                })?;
            validate_hardware_identifier(touch.peripheral())?;
            validate_hardware_identifier(touch.rtc_control())?;
            validate_hardware_identifier(touch.rtc_io())?;
            for pin in touch.pins() {
                validate_hardware_identifier(pin)?;
            }
            let prefix = format!("{}_{}", peripheral.name(), role);
            let controller =
                checked_identifier(&format!("{prefix}_controller"), &mut state.identifiers)?;
            let rtc_control =
                checked_identifier(&format!("{prefix}_rtc_control"), &mut state.identifiers)?;
            let rtc_io = checked_identifier(&format!("{prefix}_rtc_io"), &mut state.identifiers)?;
            let pin0 = checked_identifier(&format!("{prefix}_pin0"), &mut state.identifiers)?;
            let pin1 = checked_identifier(&format!("{prefix}_pin1"), &mut state.identifiers)?;
            state.raw_fields.extend([
                (
                    controller.clone(),
                    format!(
                        "::barracuda_platform_selected::__platform::hal::controller_binding_type!({})",
                        touch.peripheral()
                    ),
                ),
                (
                    rtc_control.clone(),
                    format!(
                        "::barracuda_platform_selected::__platform::hal::controller_binding_type!({})",
                        touch.rtc_control()
                    ),
                ),
                (
                    rtc_io.clone(),
                    format!(
                        "::barracuda_platform_selected::__platform::hal::controller_binding_type!({})",
                        touch.rtc_io()
                    ),
                ),
                (
                    pin0.clone(),
                    format!(
                        "::barracuda_platform_selected::__platform::hal::pin_binding_type!({})",
                        touch.pins()[0]
                    ),
                ),
                (
                    pin1.clone(),
                    format!(
                        "::barracuda_platform_selected::__platform::hal::pin_binding_type!({})",
                        touch.pins()[1]
                    ),
                ),
            ]);
            Ok((
                format!(
                    "::barracuda_platform_selected::__platform::hal::CapacitiveTouchButtons<::barracuda_platform_selected::__platform::hal::pin_binding_type!({}), ::barracuda_platform_selected::__platform::hal::pin_binding_type!({})>",
                    touch.pins()[0],
                    touch.pins()[1]
                ),
                format!(
                    "::barracuda_platform_selected::__platform::hal::capacitive_touch_buttons(bindings.{controller}, bindings.{rtc_control}, bindings.{rtc_io}, bindings.{pin0}, bindings.{pin1})"
                ),
            ))
        }
        kind => Err(GenerateError::UnsupportedBindingKind {
            implementation: peripheral.implementation().id().to_owned(),
            binding: role.to_owned(),
            kind,
        }),
    }
}

fn resolved_initial_level(
    peripheral: &ResolvedPeripheral<'_>,
    role: &str,
    schema: &BindingSchema,
) -> Result<&'static str, GenerateError> {
    let level = if let Some(level) = schema.initial() {
        level
    } else if let Some(mapping) = schema.initial_from() {
        let value = resolved_mapping_key(peripheral, &mapping.parameter)?;
        *mapping
            .values
            .get(value)
            .ok_or_else(|| GenerateError::UnsupportedResolvedValue {
                peripheral: peripheral.name().to_owned(),
                parameter: format!("{role} initial level"),
            })?
    } else {
        return Err(GenerateError::MissingResolvedParameter {
            peripheral: peripheral.name().to_owned(),
            parameter: format!("{role} initial level"),
        });
    };
    Ok(match level {
        InitialOutputLevel::Low => "Low",
        InitialOutputLevel::High => "High",
    })
}

fn resolved_mapping_key<'a>(
    peripheral: &'a ResolvedPeripheral<'_>,
    parameter: &str,
) -> Result<&'a str, GenerateError> {
    match peripheral.parameter(parameter) {
        Some(PeripheralParameter::String(value)) => Ok(value),
        Some(PeripheralParameter::Boolean(true)) => Ok("true"),
        Some(PeripheralParameter::Boolean(false)) => Ok("false"),
        _ => Err(GenerateError::MissingResolvedParameter {
            peripheral: peripheral.name().to_owned(),
            parameter: parameter.to_owned(),
        }),
    }
}

fn render_parameter(
    peripheral: &ResolvedPeripheral<'_>,
    name: &str,
    schema: &ParameterSchema,
    value: &PeripheralParameter,
) -> Result<String, GenerateError> {
    match value {
        PeripheralParameter::Boolean(value) => Ok(value.to_string()),
        PeripheralParameter::Integer(value) => Ok(value.to_string()),
        PeripheralParameter::String(value) => Ok(schema
            .rust_value(value)
            .map_or_else(|| format!("{value:?}"), str::to_owned)),
        PeripheralParameter::Sequence(_) | PeripheralParameter::Mapping(_) => {
            Err(GenerateError::UnsupportedResolvedValue {
                peripheral: peripheral.name().to_owned(),
                parameter: name.to_owned(),
            })
        }
    }
}

fn render_template(
    peripheral: &ResolvedPeripheral<'_>,
    template: &str,
    substitutions: &BTreeMap<String, String>,
) -> Result<String, GenerateError> {
    let mut rendered = String::with_capacity(template.len());
    let mut remaining = template;
    while let Some(start) = remaining.find("{{") {
        rendered.push_str(&remaining[..start]);
        let placeholder_start = start + 2;
        let Some(end) = remaining[placeholder_start..].find("}}") else {
            return Err(GenerateError::InvalidTemplate {
                implementation: peripheral.implementation().id().to_owned(),
                placeholder: remaining[start..].to_owned(),
            });
        };
        let placeholder_end = placeholder_start + end;
        let placeholder = remaining[placeholder_start..placeholder_end].trim();
        let value =
            substitutions
                .get(placeholder)
                .ok_or_else(|| GenerateError::InvalidTemplate {
                    implementation: peripheral.implementation().id().to_owned(),
                    placeholder: placeholder.to_owned(),
                })?;
        rendered.push_str(value);
        remaining = &remaining[placeholder_end + 2..];
    }
    rendered.push_str(remaining);
    Ok(rendered)
}

fn checked_identifier(
    name: &str,
    identifiers: &mut BTreeMap<String, String>,
) -> Result<String, GenerateError> {
    let identifier = rust_identifier(name);
    if let Some(first) = identifiers.insert(identifier.clone(), name.to_owned())
        && first != name
    {
        return Err(GenerateError::IdentifierCollision {
            first,
            second: name.to_owned(),
            identifier,
        });
    }
    Ok(identifier)
}

fn validate_hardware_identifier(identifier: &str) -> Result<(), GenerateError> {
    let mut bytes = identifier.bytes();
    let valid_start = bytes
        .next()
        .is_some_and(|byte| byte.is_ascii_alphabetic() || byte == b'_');
    if valid_start && bytes.all(|byte| byte.is_ascii_alphanumeric() || byte == b'_') {
        Ok(())
    } else {
        Err(GenerateError::InvalidHardwareIdentifier {
            identifier: identifier.to_owned(),
        })
    }
}

fn rust_identifier(name: &str) -> String {
    let mut result = String::with_capacity(name.len() + 1);
    for (index, character) in name.chars().enumerate() {
        let character = if character.is_ascii_alphanumeric() || character == '_' {
            character
        } else {
            '_'
        };
        if index == 0 && character.is_ascii_digit() {
            result.push('_');
        }
        result.push(character.to_ascii_lowercase());
    }
    if is_rust_keyword(&result) {
        result.insert(0, '_');
    }
    result
}

fn is_rust_keyword(identifier: &str) -> bool {
    matches!(
        identifier,
        "as" | "break"
            | "const"
            | "continue"
            | "crate"
            | "else"
            | "enum"
            | "extern"
            | "false"
            | "fn"
            | "for"
            | "if"
            | "impl"
            | "in"
            | "let"
            | "loop"
            | "match"
            | "mod"
            | "move"
            | "mut"
            | "pub"
            | "ref"
            | "return"
            | "self"
            | "Self"
            | "static"
            | "struct"
            | "super"
            | "trait"
            | "true"
            | "type"
            | "unsafe"
            | "use"
            | "where"
            | "while"
            | "async"
            | "await"
            | "dyn"
            | "abstract"
            | "become"
            | "box"
            | "do"
            | "final"
            | "macro"
            | "override"
            | "priv"
            | "typeof"
            | "unsized"
            | "virtual"
            | "yield"
            | "try"
    )
}

fn pascal_identifier(name: &str) -> String {
    let mut result = String::new();
    let mut uppercase = true;
    for character in name.chars() {
        if !character.is_ascii_alphanumeric() {
            uppercase = true;
        } else if uppercase {
            result.push(character.to_ascii_uppercase());
            uppercase = false;
        } else {
            result.push(character);
        }
    }
    if result.starts_with(|character: char| character.is_ascii_digit()) {
        result.insert(0, '_');
    }
    result
}
