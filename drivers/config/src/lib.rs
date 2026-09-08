//! Driver manifest loading and Board composition validation.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs, io,
    path::{Path, PathBuf},
};

use barracuda_board_config::{BoardDefinition, BuiltinPeripheralDefinition, PeripheralParameter};
use barracuda_platform_config::{HalBinding, PlatformDefinition};
use serde::Deserialize;

/// Driver manifest schema version supported by this toolchain.
pub const DRIVER_API_VERSION: u16 = 1;

/// All convention-discovered Driver manifests, indexed by stable ID.
#[derive(Debug)]
pub struct DriverCatalog {
    drivers: BTreeMap<String, DriverDefinition>,
}

impl DriverCatalog {
    /// Finds a Driver by the stable ID used in `board.yml`.
    #[must_use]
    pub fn driver(&self, id: &str) -> Option<&DriverDefinition> {
        self.drivers.get(id)
    }

    /// Iterates Drivers in stable ID order.
    pub fn drivers(&self) -> impl Iterator<Item = &DriverDefinition> {
        self.drivers.values()
    }
}

/// One validated `drivers/<id>/driver.yml` manifest.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct DriverDefinition {
    id: String,
    #[serde(rename = "api-version")]
    api_version: u16,
    capability: String,
    implementation: DriverImplementation,
    #[serde(default)]
    bindings: BTreeMap<String, BindingSchema>,
    #[serde(default)]
    parameters: BTreeMap<String, ParameterSchema>,
}

impl DriverDefinition {
    /// Returns the stable ID referenced by Boards.
    #[must_use]
    pub fn id(&self) -> &str {
        &self.id
    }

    /// Returns the semantic capability produced by this Driver.
    #[must_use]
    pub fn capability(&self) -> &str {
        &self.capability
    }

    /// Returns the Rust implementation mapping owned by this Driver.
    #[must_use]
    pub const fn implementation(&self) -> &DriverImplementation {
        &self.implementation
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
pub struct DriverImplementation {
    package: String,
    #[serde(rename = "crate")]
    crate_name: String,
    factory: String,
    #[serde(rename = "bindings-expression")]
    bindings_expression: String,
    #[serde(rename = "config-expression")]
    config_expression: String,
}

impl DriverImplementation {
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

    /// Returns the Driver-owned factory type template.
    #[must_use]
    pub fn factory(&self) -> &str {
        &self.factory
    }

    /// Returns the Driver-owned bindings expression template.
    #[must_use]
    pub fn bindings_expression(&self) -> &str {
        &self.bindings_expression
    }

    /// Returns the Driver-owned configuration expression template.
    #[must_use]
    pub fn config_expression(&self) -> &str {
        &self.config_expression
    }
}

/// Initial electrical level selected before enabling a digital output.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum InitialOutputLevel {
    /// Drive low before handing the output to its peripheral Driver.
    Low,
    /// Drive high before handing the output to its peripheral Driver.
    High,
}

/// Driver-owned mapping from one parameter to a safe initial output level.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct InitialOutputFromParameter {
    parameter: String,
    values: BTreeMap<String, InitialOutputLevel>,
}

impl InitialOutputFromParameter {
    /// Returns the Driver parameter that determines the initial level.
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

/// Hardware resource category accepted by one Driver binding.
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
    /// One exclusively owned SPI controller without chip select.
    SpiBus,
    /// One parallel output bus.
    ParallelOutput,
    /// One MIPI DSI host.
    DsiHost,
    /// One parallel camera receiver.
    CameraCapture,
    /// One full-duplex I2S stream.
    I2sStream,
    /// A semantic power-control capability.
    PowerControl,
    /// A semantic brightness-control capability.
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

/// Portable value category accepted by one Driver parameter.
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

/// Schema for one Driver initialization parameter.
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

    /// Returns the optional default supplied by the Driver.
    #[must_use]
    pub const fn default(&self) -> Option<&PeripheralParameter> {
        self.default.as_ref()
    }

    fn rust_value(&self, value: &str) -> Option<&str> {
        self.rust_values.get(value).map(String::as_str)
    }
}

/// A Board peripheral paired with its validated Driver manifest.
#[derive(Debug)]
pub struct ResolvedPeripheral<'a> {
    name: &'a str,
    definition: &'a BuiltinPeripheralDefinition,
    driver: &'a DriverDefinition,
}

impl<'a> ResolvedPeripheral<'a> {
    /// Returns the Board-level peripheral name.
    #[must_use]
    pub fn name(&self) -> &str {
        self.name
    }

    /// Returns the resolved Driver definition.
    #[must_use]
    pub const fn driver(&self) -> &DriverDefinition {
        self.driver
    }

    /// Returns one validated chip-native binding identifier.
    #[must_use]
    pub fn binding(&self, role: &str) -> Option<&str> {
        self.definition.binding(role)
    }

    /// Returns one validated Board parameter, or its Driver-owned default.
    #[must_use]
    pub fn parameter(&self, name: &str) -> Option<&PeripheralParameter> {
        self.definition.parameter(name).or_else(|| {
            self.driver
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

/// Failure while discovering or validating Driver manifests.
#[derive(Debug, thiserror::Error)]
pub enum CatalogError {
    /// The Driver catalog directory could not be read.
    #[error("failed to read Driver catalog `{path}`: {source}")]
    Io {
        /// Path that failed.
        path: PathBuf,
        /// Underlying filesystem failure.
        #[source]
        source: io::Error,
    },
    /// A Driver manifest has invalid YAML or structure.
    #[error("invalid Driver manifest `{path}`: {message}")]
    Manifest {
        /// Manifest path.
        path: PathBuf,
        /// Parser or validation detail.
        message: String,
    },
    /// Directory name and stable Driver ID differ.
    #[error("Driver directory `{directory}` declares ID `{declared}`")]
    IdMismatch {
        /// Directory component.
        directory: String,
        /// Manifest ID.
        declared: String,
    },
    /// More than one manifest declares the same stable ID.
    #[error("duplicate Driver ID `{id}`")]
    DuplicateId {
        /// Duplicated ID.
        id: String,
    },
}

/// Failure while resolving a Board against Driver-owned schemas.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ResolveError {
    /// Board references an undiscovered stable Driver ID.
    #[error("Board peripheral `{peripheral}` references unknown Driver `{driver}`")]
    UnknownDriver {
        /// Board-level peripheral name.
        peripheral: String,
        /// Unknown Driver ID.
        driver: String,
    },
    /// A required Driver binding is missing.
    #[error("Board peripheral `{peripheral}` is missing Driver binding `{binding}`")]
    MissingBinding {
        /// Board-level peripheral name.
        peripheral: String,
        /// Missing binding role.
        binding: String,
    },
    /// Board supplies a binding absent from the Driver schema.
    #[error("Board peripheral `{peripheral}` supplies unknown Driver binding `{binding}`")]
    UnknownBinding {
        /// Board-level peripheral name.
        peripheral: String,
        /// Unknown binding role.
        binding: String,
    },
    /// A required Driver parameter is missing.
    #[error("Board peripheral `{peripheral}` is missing Driver parameter `{parameter}`")]
    MissingParameter {
        /// Board-level peripheral name.
        peripheral: String,
        /// Missing parameter name.
        parameter: String,
    },
    /// Board supplies a parameter absent from the Driver schema.
    #[error("Board peripheral `{peripheral}` supplies unknown Driver parameter `{parameter}`")]
    UnknownParameter {
        /// Board-level peripheral name.
        peripheral: String,
        /// Unknown parameter name.
        parameter: String,
    },
    /// A Board parameter does not satisfy the Driver schema.
    #[error("Board peripheral `{peripheral}` has invalid Driver parameter `{parameter}`")]
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
        /// Driver-defined binding role.
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
    /// A capability has not defined its generated Board storage contract.
    #[error("Driver `{driver}` produces unsupported generated capability `{capability}`")]
    UnsupportedCapability {
        /// Driver ID.
        driver: String,
        /// Capability ID.
        capability: String,
    },
    /// Current generated access API allows only one primary capability of a kind.
    #[error("Board declares more than one primary `{capability}` capability")]
    DuplicatePrimaryCapability {
        /// Duplicated capability ID.
        capability: String,
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
    /// A generated capability currently requires a binding of another kind.
    #[error("Driver `{driver}` binding `{binding}` has unsupported kind {kind:?}")]
    UnsupportedBindingKind {
        /// Driver ID.
        driver: String,
        /// Binding role.
        binding: String,
        /// Unsupported category.
        kind: BindingKind,
    },
    /// A Driver-owned composition template contains an unknown placeholder.
    #[error("Driver `{driver}` composition template contains unknown placeholder `{placeholder}`")]
    InvalidTemplate {
        /// Driver whose template failed.
        driver: String,
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

/// Discovers `drivers/*/driver.yml` using directory names as stable IDs.
///
/// Directories without a manifest are ignored, allowing support crates to live
/// beside concrete Drivers.
///
/// # Errors
///
/// Returns [`CatalogError`] for filesystem, YAML, identity, or schema failures.
pub fn load_catalog(workspace_root: &Path) -> Result<DriverCatalog, CatalogError> {
    let root = workspace_root.join("drivers");
    let entries = fs::read_dir(&root).map_err(|source| CatalogError::Io {
        path: root.clone(),
        source,
    })?;
    let mut drivers = BTreeMap::new();
    for entry in entries {
        let entry = entry.map_err(|source| CatalogError::Io {
            path: root.clone(),
            source,
        })?;
        let file_type = entry.file_type().map_err(|source| CatalogError::Io {
            path: entry.path(),
            source,
        })?;
        if !file_type.is_dir() {
            continue;
        }
        let Ok(directory) = entry.file_name().into_string() else {
            continue;
        };
        let path = entry.path().join("driver.yml");
        let yaml = match fs::read_to_string(&path) {
            Ok(yaml) => yaml,
            Err(source) if source.kind() == io::ErrorKind::NotFound => continue,
            Err(source) => return Err(CatalogError::Io { path, source }),
        };
        let driver = parse_manifest(&path, &yaml)?;
        if driver.id != directory {
            return Err(CatalogError::IdMismatch {
                directory,
                declared: driver.id,
            });
        }
        let id = driver.id.clone();
        if drivers.insert(id.clone(), driver).is_some() {
            return Err(CatalogError::DuplicateId { id });
        }
    }
    Ok(DriverCatalog { drivers })
}

fn parse_manifest(path: &Path, yaml: &str) -> Result<DriverDefinition, CatalogError> {
    let mut documents = yaml_peg::serde::from_str::<DriverDefinition>(yaml).map_err(|error| {
        CatalogError::Manifest {
            path: path.to_owned(),
            message: error.to_string(),
        }
    })?;
    if documents.len() != 1 {
        return Err(CatalogError::Manifest {
            path: path.to_owned(),
            message: format!("expected one YAML document, found {}", documents.len()),
        });
    }
    let driver = documents.pop().ok_or_else(|| CatalogError::Manifest {
        path: path.to_owned(),
        message: String::from("expected one YAML document, found 0"),
    })?;
    validate_manifest(path, &driver)?;
    Ok(driver)
}

fn validate_manifest(path: &Path, driver: &DriverDefinition) -> Result<(), CatalogError> {
    let valid_id = !driver.id.is_empty()
        && driver
            .id
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-');
    let valid_crate = !driver.implementation.crate_name.is_empty()
        && driver
            .implementation
            .crate_name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_');
    let valid_names = driver
        .bindings
        .keys()
        .chain(driver.parameters.keys())
        .all(|name| !name.trim().is_empty());
    let valid_enums = driver
        .parameters
        .values()
        .all(|schema| schema.parameter_type != ParameterType::Enum || !schema.values.is_empty());
    if !valid_id
        || driver.api_version != DRIVER_API_VERSION
        || driver.capability.trim().is_empty()
        || driver.implementation.package.trim().is_empty()
        || !valid_crate
        || driver.implementation.factory.trim().is_empty()
        || driver.implementation.bindings_expression.trim().is_empty()
        || driver.implementation.config_expression.trim().is_empty()
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
        driver.implementation.factory(),
        driver.implementation.bindings_expression(),
        driver.implementation.config_expression(),
    ] {
        validate_template_placeholders(driver, template).map_err(|message| {
            CatalogError::Manifest {
                path: path.to_owned(),
                message,
            }
        })?;
    }
    for (name, schema) in &driver.parameters {
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
    for (role, schema) in &driver.bindings {
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
            let Some(parameter) = driver.parameter(&mapping.parameter) else {
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

fn validate_template_placeholders(driver: &DriverDefinition, template: &str) -> Result<(), String> {
    let mut remaining = template;
    while let Some(start) = remaining.find("{{") {
        let placeholder_start = start + 2;
        let Some(end) = remaining[placeholder_start..].find("}}") else {
            return Err(format!(
                "Driver `{}` has an unterminated composition placeholder",
                driver.id()
            ));
        };
        let placeholder_end = placeholder_start + end;
        let placeholder = remaining[placeholder_start..placeholder_end].trim();
        let valid = placeholder == "crate"
            || matches!(placeholder, "hal.delay.type" | "hal.delay.value")
            || placeholder
                .strip_prefix("parameter.")
                .is_some_and(|name| driver.parameter(name).is_some())
            || placeholder
                .strip_prefix("binding.")
                .and_then(|value| value.rsplit_once('.'))
                .is_some_and(|(role, part)| {
                    matches!(part, "type" | "value") && driver.binding(role).is_some()
                });
        if !valid {
            return Err(format!(
                "Driver `{}` composition template contains unknown placeholder `{placeholder}`",
                driver.id()
            ));
        }
        remaining = &remaining[placeholder_end + 2..];
    }
    Ok(())
}

/// Validates every Board built-in against the referenced Driver schema.
///
/// # Errors
///
/// Returns [`ResolveError`] for unknown IDs, schema mismatches, missing values,
/// or values of the wrong type.
pub fn resolve_board<'a>(
    board: &'a BoardDefinition,
    catalog: &'a DriverCatalog,
) -> Result<ResolvedBoard<'a>, ResolveError> {
    let mut peripherals = Vec::with_capacity(board.builtin_peripheral_count());
    for (name, definition) in board.builtin_peripherals() {
        let driver =
            catalog
                .driver(definition.driver())
                .ok_or_else(|| ResolveError::UnknownDriver {
                    peripheral: name.to_owned(),
                    driver: definition.driver().to_owned(),
                })?;
        for (role, schema) in driver.bindings() {
            if schema.required() && definition.binding(role).is_none() {
                return Err(ResolveError::MissingBinding {
                    peripheral: name.to_owned(),
                    binding: role.to_owned(),
                });
            }
            if let Some(resource) = definition.binding(role)
                && ((schema.kind() == BindingKind::SpiDevice
                    && board.internal_io().spi_device(resource).is_none())
                    || (schema.kind() == BindingKind::SpiBus
                        && board.internal_io().spi_bus(resource).is_none())
                    || (schema.kind() == BindingKind::I2cDevice
                        && board.internal_io().i2c_device(resource).is_none())
                    || (schema.kind() == BindingKind::CameraCapture
                        && board.internal_io().camera_capture(resource).is_none())
                    || (schema.kind() == BindingKind::I2sStream
                        && board.internal_io().i2s_stream(resource).is_none()))
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
            if driver.binding(role).is_none() {
                return Err(ResolveError::UnknownBinding {
                    peripheral: name.to_owned(),
                    binding: role.to_owned(),
                });
            }
        }
        for (parameter, schema) in &driver.parameters {
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
            if driver.parameter(parameter).is_none() {
                return Err(ResolveError::UnknownParameter {
                    peripheral: name.to_owned(),
                    parameter: parameter.to_owned(),
                });
            }
        }
        peripherals.push(ResolvedPeripheral {
            name,
            definition,
            driver,
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

/// Renders the selected Board HAL from validated Board and Driver manifests.
///
/// The source contains concrete Platform HAL and Driver types. It performs no
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
             pub type SelectedBoardHal = ::barracuda_board_hal::EmptyBoardHal;\n",
        ));
    }
    render_generic_hal(board, resolved, &[], &[])
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
        platform.hal().runtime_i2c_controllers(),
        platform.hal().runtime_spi_controllers(),
    )
}

/// Validates that a selected Platform HAL can construct every Board resource.
///
/// # Errors
///
/// Returns [`GenerateError::UnsupportedHalBinding`] when the Platform manifest
/// omits a binding form required by the Board or one of its Drivers.
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
        for (role, schema) in peripheral.driver().bindings() {
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
        BindingKind::SpiBus => Some(HalBinding::SpiBus),
        BindingKind::I2cDevice => Some(HalBinding::I2cDevice),
        BindingKind::CameraCapture => Some(HalBinding::CameraCapture),
        BindingKind::I2sStream => Some(HalBinding::I2sStream),
        _ => None,
    }
}

#[derive(Debug)]
struct RenderedPeripheral {
    name: String,
    field: String,
    alias: String,
    error_variant: String,
    capability: String,
    factory: String,
    bindings: String,
    config: String,
}

#[derive(Default)]
struct RenderState {
    raw_fields: Vec<(String, String)>,
    binding_errors: Vec<(String, String)>,
    identifiers: BTreeMap<String, String>,
}

fn render_generic_hal(
    board: &BoardDefinition,
    resolved: &ResolvedBoard<'_>,
    runtime_i2c_controllers: &[String],
    runtime_spi_controllers: &[String],
) -> Result<String, GenerateError> {
    let mut state = RenderState::default();
    let mut rendered = Vec::new();
    let mut primary_capabilities = BTreeSet::new();

    for peripheral in resolved.peripherals() {
        let field = checked_identifier(peripheral.name(), &mut state.identifiers)?;
        if matches!(
            peripheral.driver().capability(),
            "display" | "indicator" | "led-strip" | "camera" | "audio-codec"
        ) && !primary_capabilities.insert(peripheral.driver().capability())
        {
            return Err(GenerateError::DuplicatePrimaryCapability {
                capability: peripheral.driver().capability().to_owned(),
            });
        }

        let mut substitutions = BTreeMap::new();
        substitutions.insert(
            String::from("crate"),
            peripheral.driver().implementation().crate_name().to_owned(),
        );
        substitutions.insert(
            String::from("hal.delay.type"),
            String::from("::barracuda_platform_selected::__platform::hal::DriverDelay"),
        );
        substitutions.insert(
            String::from("hal.delay.value"),
            String::from("::barracuda_platform_selected::__platform::hal::delay()"),
        );
        for (role, schema) in peripheral.driver().bindings() {
            let Some(resource) = peripheral.binding(role) else {
                continue;
            };
            let rendered_binding =
                render_binding(board, peripheral, role, schema, resource, &mut state)?;
            substitutions.insert(format!("binding.{role}.type"), rendered_binding.0);
            substitutions.insert(format!("binding.{role}.value"), rendered_binding.1);
        }
        for (name, schema) in &peripheral.driver().parameters {
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

        let implementation = peripheral.driver().implementation();
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
            alias: format!("{}Capability", pascal_identifier(peripheral.name())),
            error_variant: pascal_identifier(peripheral.name()),
            capability: peripheral.driver().capability().to_owned(),
            field,
            factory,
            bindings,
            config,
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
            .filter(|controller| !board.internal_io().uses_controller(controller))
            .collect::<Vec<_>>()
    };
    let runtime_spi_controllers = if pin_count == 0 {
        Vec::new()
    } else {
        runtime_spi_controllers
            .iter()
            .filter(|controller| !board.internal_io().uses_controller(controller))
            .collect::<Vec<_>>()
    };
    for controller in runtime_i2c_controllers
        .iter()
        .chain(runtime_spi_controllers.iter())
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

    let mut source = String::from(
        "mod generated_board_hal {\n\
         use core::fmt;\n\
         use ::barracuda_board_hal::{BoardHal, BoardHalInitResult, BoardHalResources, PeripheralDriver};\n\
         use ::embassy_executor::Spawner;\n\n\
         pub struct GeneratedBoardBindings {\n",
    );
    for (field, ty) in &state.raw_fields {
        source.push_str(&format!("    {field}: {ty},\n"));
    }
    source.push_str("}\n\nimpl GeneratedBoardBindings {\n    #[must_use]\n    pub const fn new(\n");
    for (field, ty) in &state.raw_fields {
        source.push_str(&format!("        {field}: {ty},\n"));
    }
    source.push_str("    ) -> Self {\n        Self {\n");
    for (field, _) in &state.raw_fields {
        source.push_str(&format!("            {field},\n"));
    }
    source.push_str("        }\n    }\n}\n\n");

    for peripheral in &rendered {
        source.push_str(&format!(
            "type {}Factory = {};\npub type {} = <{}Factory as PeripheralDriver>::Capability;\n\n",
            peripheral.error_variant,
            peripheral.factory,
            peripheral.alias,
            peripheral.error_variant,
        ));
    }
    if rendered.is_empty() {
        source.push_str(
            "pub type GeneratedBuiltins = ::barracuda_board_hal::NoBuiltinCapabilities;\n\n",
        );
    } else {
        source.push_str("pub struct GeneratedBuiltins {\n");
        for peripheral in &rendered {
            source.push_str(&format!(
                "    {}: Option<{}>,\n",
                peripheral.field, peripheral.alias
            ));
        }
        source.push_str("}\n\nimpl GeneratedBuiltins {\n");
        for peripheral in &rendered {
            source.push_str(&format!(
                "    pub fn take_{}(&mut self) -> Option<{}> {{ self.{}.take() }}\n",
                peripheral.field, peripheral.alias, peripheral.field
            ));
        }
        source.push_str("}\n\n");
        for peripheral in &rendered {
            match peripheral.capability.as_str() {
                "display" => source.push_str(&format!(
                    "impl ::barracuda_board_hal::display::BuiltinDisplay for GeneratedBuiltins {{\n    type Display = {};\n    fn take_display(&mut self) -> Option<Self::Display> {{ self.{}.take() }}\n}}\n\n",
                    peripheral.alias, peripheral.field
                )),
                "indicator" => source.push_str(&format!(
                    "impl ::barracuda_board_hal::indicator::BuiltinIndicator for GeneratedBuiltins {{\n    type Indicator = {};\n    fn take_indicator(&mut self) -> Option<Self::Indicator> {{ self.{}.take() }}\n}}\n\n",
                    peripheral.alias, peripheral.field
                )),
                "led-strip" => source.push_str(&format!(
                    "impl ::barracuda_board_hal::led_strip::BuiltinLedStrip for GeneratedBuiltins {{\n    type LedStrip = {};\n    fn take_led_strip(&mut self) -> Option<Self::LedStrip> {{ self.{}.take() }}\n}}\n\n",
                    peripheral.alias, peripheral.field
                )),
                "camera" => source.push_str(&format!(
                    "impl ::barracuda_board_hal::camera::BuiltinCamera for GeneratedBuiltins {{\n    type Camera = {};\n    fn take_camera(&mut self) -> Option<Self::Camera> {{ self.{}.take() }}\n}}\n\n",
                    peripheral.alias, peripheral.field
                )),
                "audio-codec" => source.push_str(&format!(
                    "impl ::barracuda_board_hal::audio::BuiltinAudioCodec for GeneratedBuiltins {{\n    type AudioCodec = {};\n    fn take_audio_codec(&mut self) -> Option<Self::AudioCodec> {{ self.{}.take() }}\n}}\n\n",
                    peripheral.alias, peripheral.field
                )),
                _ => {}
            }
        }
        if !primary_capabilities.contains("led-strip") {
            source.push_str(
                "impl ::barracuda_board_hal::led_strip::BuiltinLedStrip for GeneratedBuiltins {\n    type LedStrip = ::barracuda_board_hal::UnavailableLedStrip;\n    fn take_led_strip(&mut self) -> Option<Self::LedStrip> { None }\n}\n\n",
            );
        }
        if !primary_capabilities.contains("camera") {
            source.push_str(
                "impl ::barracuda_board_hal::camera::BuiltinCamera for GeneratedBuiltins {\n    type Camera = ::barracuda_board_hal::UnavailableCamera;\n    fn take_camera(&mut self) -> Option<Self::Camera> { None }\n}\n\n",
            );
        }
        if !primary_capabilities.contains("display") {
            source.push_str(
                "impl ::barracuda_board_hal::display::BuiltinDisplay for GeneratedBuiltins {\n    type Display = ::barracuda_board_hal::UnavailableDisplay;\n    fn take_display(&mut self) -> Option<Self::Display> { None }\n}\n\n",
            );
        }
        if !primary_capabilities.contains("audio-codec") {
            source.push_str(
                "impl ::barracuda_board_hal::audio::BuiltinAudioCodec for GeneratedBuiltins {\n    type AudioCodec = ::barracuda_board_hal::UnavailableAudioCodec;\n    fn take_audio_codec(&mut self) -> Option<Self::AudioCodec> { None }\n}\n\n",
            );
        }
    }

    source.push_str(&format!(
        "pub type GeneratedIo = ::barracuda_platform_selected::__platform::hal::RuntimeIo<{pin_count}, {}, {}>;\n\n",
        runtime_i2c_controllers.len(),
        runtime_spi_controllers.len(),
    ));

    if rendered.is_empty() && state.binding_errors.is_empty() {
        source.push_str("type GeneratedBoardError = core::convert::Infallible;\n\n");
    } else {
        source.push_str("#[derive(Debug)]\npub enum GeneratedBoardError {\n");
        for (variant, ty) in &state.binding_errors {
            source.push_str(&format!("    {variant}({ty}),\n"));
        }
        for peripheral in &rendered {
            source.push_str(&format!(
                "    {}(<{}Factory as PeripheralDriver>::Error),\n",
                peripheral.error_variant, peripheral.error_variant
            ));
        }
        source.push_str("}\n\nimpl fmt::Display for GeneratedBoardError {\n    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {\n        match self {\n");
        for (variant, _) in &state.binding_errors {
            source.push_str(&format!(
                "            Self::{variant}(_) => formatter.write_str({:?}),\n",
                format!("failed to construct `{}` binding", variant)
            ));
        }
        for peripheral in &rendered {
            source.push_str(&format!(
                "            Self::{}(_) => formatter.write_str({:?}),\n",
                peripheral.error_variant,
                format!("failed to initialize `{}`", peripheral.name)
            ));
        }
        source.push_str(
            "        }\n    }\n}\n\nimpl core::error::Error for GeneratedBoardError {}\n\n",
        );
    }

    source.push_str(
        "pub struct SelectedBoardHal;\n\nimpl BoardHal for SelectedBoardHal {\n    type Bindings = GeneratedBoardBindings;\n    type Resources = BoardHalResources<GeneratedBuiltins, GeneratedIo>;\n    type Error = GeneratedBoardError;\n\n    async fn initialize(_spawner: Spawner, bindings: Self::Bindings) -> BoardHalInitResult<Self> {\n",
    );
    for peripheral in &rendered {
        source.push_str(&format!(
            "        let {} = <{}Factory as PeripheralDriver>::initialize({}, {}).await.map_err(GeneratedBoardError::{})?;\n",
            peripheral.field,
            peripheral.error_variant,
            peripheral.bindings,
            peripheral.config,
            peripheral.error_variant,
        ));
    }
    if rendered.is_empty() {
        source.push_str("        let builtins = ::barracuda_board_hal::NoBuiltinCapabilities;\n");
    } else {
        source.push_str("        let builtins = GeneratedBuiltins {\n");
        for peripheral in &rendered {
            source.push_str(&format!(
                "            {}: Some({}),\n",
                peripheral.field, peripheral.field
            ));
        }
        source.push_str("        };\n");
    }
    source.push_str(
        "        let io = ::barracuda_platform_selected::__platform::hal::runtime_io([\n",
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
    source.push_str("        ]);\n");
    source.push_str("        Ok(BoardHalResources::new(builtins, io))\n    }\n}\n}\n\n");
    source.push_str("pub use generated_board_hal::*;\n");
    Ok(source)
}

fn render_binding(
    board: &BoardDefinition,
    peripheral: &ResolvedPeripheral<'_>,
    role: &str,
    schema: &BindingSchema,
    resource: &str,
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
            let spi = board.internal_io().spi_device(resource).ok_or_else(|| {
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
        BindingKind::SpiBus => {
            let spi = board.internal_io().spi_bus(resource).ok_or_else(|| {
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
        BindingKind::I2cDevice => {
            let i2c = board.internal_io().i2c_device(resource).ok_or_else(|| {
                GenerateError::UnsupportedResolvedValue {
                    peripheral: peripheral.name().to_owned(),
                    parameter: format!("{role} binding"),
                }
            })?;
            for identifier in [i2c.peripheral(), i2c.scl(), i2c.sda()] {
                validate_hardware_identifier(identifier)?;
            }
            let prefix = format!("{}_{}", peripheral.name(), role);
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
            let variant = format!(
                "{}{}Binding",
                pascal_identifier(peripheral.name()),
                pascal_identifier(role)
            );
            state.binding_errors.push((
                variant.clone(),
                String::from("::barracuda_platform_selected::__platform::hal::I2cConfigError"),
            ));
            Ok((
                String::from("::barracuda_platform_selected::__platform::hal::I2cBus"),
                format!(
                    "::barracuda_platform_selected::__platform::hal::i2c_device(bindings.{controller}, bindings.{scl}, bindings.{sda}, {}).map_err(GeneratedBoardError::{variant})?",
                    i2c.frequency_hz()
                ),
            ))
        }
        BindingKind::CameraCapture => {
            let camera = board
                .internal_io()
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
            let stream = board.internal_io().i2s_stream(resource).ok_or_else(|| {
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
        kind => Err(GenerateError::UnsupportedBindingKind {
            driver: peripheral.driver().id().to_owned(),
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
                driver: peripheral.driver().id().to_owned(),
                placeholder: remaining[start..].to_owned(),
            });
        };
        let placeholder_end = placeholder_start + end;
        let placeholder = remaining[placeholder_start..placeholder_end].trim();
        let value =
            substitutions
                .get(placeholder)
                .ok_or_else(|| GenerateError::InvalidTemplate {
                    driver: peripheral.driver().id().to_owned(),
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
