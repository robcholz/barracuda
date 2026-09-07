//! Driver manifest loading and Board composition validation.

use std::{
    collections::BTreeMap,
    fs, io,
    path::{Path, PathBuf},
};

use barracuda_board_config::{BoardDefinition, BuiltinPeripheralDefinition, PeripheralParameter};
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
    #[serde(rename = "driver-type")]
    driver_type: String,
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

    /// Returns the public Driver factory type path inside the crate.
    #[must_use]
    pub fn driver_type(&self) -> &str {
        &self.driver_type
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
    /// One parallel output bus.
    ParallelOutput,
    /// One MIPI DSI host.
    DsiHost,
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
    /// The selected chip has no static Board binding adapter yet.
    #[error("chip `{chip}` has no Board binding adapter")]
    UnsupportedChip {
        /// Unsupported canonical chip ID.
        chip: String,
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
    /// A schema-valid value cannot be represented by the selected chip adapter.
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
        || driver.implementation.driver_type.trim().is_empty()
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
    for (name, schema) in &driver.parameters {
        if let Some(default) = &schema.default
            && !parameter_matches(schema, default)
        {
            return Err(CatalogError::Manifest {
                path: path.to_owned(),
                message: format!("default for parameter `{name}` does not match its schema"),
            });
        }
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
                && schema.kind() == BindingKind::SpiDevice
                && board.internal_io().spi_device(resource).is_none()
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
/// The source contains concrete chip and Driver types. It performs no runtime
/// lookup and introduces no trait objects or heap allocation.
///
/// # Errors
///
/// Returns [`GenerateError`] when the chip adapter or a declared capability is
/// not supported by the static generator.
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
    match board.hardware().chip() {
        "stm32f429zi" => render_stm32f429zi_hal(board, resolved),
        "esp32" => render_esp_hal(board, resolved, "esp32"),
        "esp32s3" => render_esp_hal(board, resolved, "esp32s3"),
        chip => Err(GenerateError::UnsupportedChip {
            chip: chip.to_owned(),
        }),
    }
}

fn render_stm32f429zi_hal(
    board: &BoardDefinition,
    resolved: &ResolvedBoard<'_>,
) -> Result<String, GenerateError> {
    let indicators = resolved
        .peripherals()
        .filter(|peripheral| peripheral.driver().capability() == "indicator")
        .collect::<Vec<_>>();
    if indicators.len() > 1 {
        return Err(GenerateError::DuplicatePrimaryCapability {
            capability: String::from("indicator"),
        });
    }
    for peripheral in resolved.peripherals() {
        if peripheral.driver().capability() != "indicator" {
            return Err(GenerateError::UnsupportedCapability {
                driver: peripheral.driver().id().to_owned(),
                capability: peripheral.driver().capability().to_owned(),
            });
        }
    }

    let mut binding_fields = Vec::new();
    let mut binding_arguments = Vec::new();
    for peripheral in resolved.peripherals() {
        for (role, schema) in peripheral.driver().bindings() {
            if schema.kind() != BindingKind::DigitalOutput {
                return Err(GenerateError::UnsupportedBindingKind {
                    driver: peripheral.driver().id().to_owned(),
                    binding: role.to_owned(),
                    kind: schema.kind(),
                });
            }
            let Some(identifier) = peripheral.binding(role) else {
                continue;
            };
            validate_hardware_identifier(identifier)?;
            let field = rust_identifier(&format!("{}_{}", peripheral.name(), role));
            let ty = format!(
                "::barracuda_chip_stm32f429zi::Peri<'static, ::barracuda_chip_stm32f429zi::peripherals::{identifier}>"
            );
            binding_fields.push((field, ty));
        }
    }
    for (name, gpio) in board.exposed_io().gpios() {
        validate_hardware_identifier(gpio.pin())?;
        let field = rust_identifier(&format!("gpio_{name}"));
        let ty = format!(
            "::barracuda_chip_stm32f429zi::Peri<'static, ::barracuda_chip_stm32f429zi::peripherals::{}>",
            gpio.pin()
        );
        binding_fields.push((field, ty));
    }
    for (field, ty) in &binding_fields {
        binding_arguments.push(format!("{field}: {ty}"));
    }

    let gpio_count = board.exposed_io().gpios().count();
    let mut source = String::from(
        "#[cfg(target_arch = \"arm\")]\nmod generated_board_hal {\n\
         use core::convert::Infallible;\n\
         use ::barracuda_board_hal::{BoardHal, BoardHalInitResult, BoardHalResources, ExposedIo, NamedResources, UnavailableI2c, UnavailableSpi};\n\
         use ::barracuda_board_hal::{indicator::{ActiveLevel, BuiltinIndicator, IndicatorConfig}, PeripheralDriver};\n\
         use ::embassy_executor::Spawner;\n\n\
         pub struct GeneratedBoardBindings {\n",
    );
    for (field, ty) in &binding_fields {
        source.push_str(&format!("    {field}: {ty},\n"));
    }
    source.push_str("}\n\nimpl GeneratedBoardBindings {\n    #[must_use]\n    pub const fn new(\n");
    for argument in &binding_arguments {
        source.push_str(&format!("        {argument},\n"));
    }
    source.push_str("    ) -> Self {\n        Self {\n");
    for (field, _) in &binding_fields {
        source.push_str(&format!("            {field},\n"));
    }
    source.push_str("        }\n    }\n}\n\n");

    if let Some(indicator) = indicators.first() {
        let implementation = indicator.driver().implementation();
        let alias = format!("{}Capability", pascal_identifier(indicator.name()));
        let factory = format!(
            "::{}::{}<::barracuda_chip_stm32f429zi::DigitalOutput>",
            implementation.crate_name(),
            implementation.driver_type()
        );
        source.push_str(&format!(
            "type {alias} = <{factory} as PeripheralDriver>::Capability;\n\n\
             pub struct GeneratedBuiltins {{\n    {}: Option<{alias}>,\n}}\n\n",
            rust_identifier(indicator.name())
        ));
        source.push_str(&format!(
            "impl BuiltinIndicator for GeneratedBuiltins {{\n    type Indicator = {alias};\n\n    fn take_indicator(&mut self) -> Option<Self::Indicator> {{\n        self.{}.take()\n    }}\n}}\n\n",
            rust_identifier(indicator.name())
        ));
    } else {
        source.push_str(
            "pub type GeneratedBuiltins = ::barracuda_board_hal::NoBuiltinCapabilities;\n\n",
        );
    }

    source.push_str(&format!(
        "pub struct GeneratedIo {{\n    gpio: Option<NamedResources<::barracuda_chip_stm32f429zi::DynamicPin, {gpio_count}>>,\n}}\n\n\
         impl ExposedIo for GeneratedIo {{\n    type Gpio = NamedResources<::barracuda_chip_stm32f429zi::DynamicPin, {gpio_count}>;\n    type I2c = NamedResources<UnavailableI2c, 0>;\n    type Spi = NamedResources<UnavailableSpi, 0>;\n\n    fn take_gpio(&mut self) -> Option<Self::Gpio> {{ self.gpio.take() }}\n    fn take_i2c(&mut self) -> Option<Self::I2c> {{ None }}\n    fn take_spi(&mut self) -> Option<Self::Spi> {{ None }}\n}}\n\n\
         pub struct SelectedBoardHal;\n\n\
         impl BoardHal for SelectedBoardHal {{\n    type Bindings = GeneratedBoardBindings;\n    type Resources = BoardHalResources<GeneratedBuiltins, GeneratedIo>;\n    type Error = Infallible;\n\n    async fn initialize(_spawner: Spawner, bindings: Self::Bindings) -> BoardHalInitResult<Self> {{\n"
    ));
    if let Some(indicator) = indicators.first() {
        let implementation = indicator.driver().implementation();
        let field = rust_identifier(indicator.name());
        let binding_field = rust_identifier(&format!("{}_pin", indicator.name()));
        let active = match indicator.parameter("active-level") {
            Some(PeripheralParameter::String(value)) if value == "low" => "Low",
            Some(PeripheralParameter::String(value)) if value == "high" => "High",
            _ => {
                return Err(GenerateError::MissingResolvedParameter {
                    peripheral: indicator.name().to_owned(),
                    parameter: String::from("active-level"),
                });
            }
        };
        let initial = if active == "High" { "Low" } else { "High" };
        let factory = format!(
            "::{}::{}<::barracuda_chip_stm32f429zi::DigitalOutput>",
            implementation.crate_name(),
            implementation.driver_type()
        );
        source.push_str(&format!(
            "        let {field} = <{factory} as PeripheralDriver>::initialize(\n            ::barracuda_chip_stm32f429zi::digital_output(bindings.{binding_field}, ::barracuda_board_hal::DigitalLevel::{initial}),\n            IndicatorConfig::new(ActiveLevel::{active}),\n        ).await;\n        let {field} = match {field} {{ Ok(value) => value, Err(never) => match never {{}} }};\n        let builtins = GeneratedBuiltins {{ {field}: Some({field}) }};\n"
        ));
    } else {
        source.push_str("        let builtins = ::barracuda_board_hal::NoBuiltinCapabilities;\n");
    }
    if gpio_count == 0 {
        source.push_str("        let gpio = NamedResources::empty();\n");
    } else {
        source.push_str("        let gpio = NamedResources::new([\n");
        for (name, _) in board.exposed_io().gpios() {
            let field = rust_identifier(&format!("gpio_{name}"));
            source.push_str(&format!(
                "            ({name:?}, ::barracuda_chip_stm32f429zi::dynamic_pin(bindings.{field})),\n"
            ));
        }
        source.push_str("        ]);\n");
    }
    source.push_str(
        "        Ok(BoardHalResources::new(builtins, GeneratedIo { gpio: Some(gpio) }))\n    }\n}\n}\n\n#[cfg(target_arch = \"arm\")]\npub use generated_board_hal::*;\n",
    );
    Ok(source)
}

fn render_esp_hal(
    board: &BoardDefinition,
    resolved: &ResolvedBoard<'_>,
    chip: &str,
) -> Result<String, GenerateError> {
    if !board.exposed_io().is_empty() {
        return Err(GenerateError::UnsupportedCapability {
            driver: String::from("board"),
            capability: format!("{chip}-exposed-io"),
        });
    }
    let displays = resolved
        .peripherals()
        .filter(|peripheral| peripheral.driver().capability() == "display")
        .collect::<Vec<_>>();
    if displays.len() > 1 {
        return Err(GenerateError::DuplicatePrimaryCapability {
            capability: String::from("display"),
        });
    }
    let display = displays
        .first()
        .ok_or_else(|| GenerateError::UnsupportedCapability {
            driver: String::from("board"),
            capability: format!("{chip}-internal-io-without-display"),
        })?;
    if display.driver().id() == "gdeh0154d67-display" {
        return render_esp_gdeh0154d67_hal(board, display, chip);
    }
    if display.driver().id() != "mipi-dbi-display" || resolved.peripherals().count() != 1 {
        return Err(GenerateError::UnsupportedCapability {
            driver: display.driver().id().to_owned(),
            capability: display.driver().capability().to_owned(),
        });
    }
    let spi_name =
        display
            .binding("spi")
            .ok_or_else(|| GenerateError::MissingResolvedParameter {
                peripheral: display.name().to_owned(),
                parameter: String::from("spi binding"),
            })?;
    let spi = board.internal_io().spi_device(spi_name).ok_or_else(|| {
        GenerateError::UnsupportedResolvedValue {
            peripheral: display.name().to_owned(),
            parameter: String::from("spi binding"),
        }
    })?;
    let mosi = spi
        .mosi()
        .ok_or_else(|| GenerateError::UnsupportedResolvedValue {
            peripheral: display.name().to_owned(),
            parameter: String::from("spi.mosi"),
        })?;
    if spi.miso().is_some() {
        return Err(GenerateError::UnsupportedResolvedValue {
            peripheral: display.name().to_owned(),
            parameter: String::from("spi.miso"),
        });
    }
    let dc = display
        .binding("dc")
        .ok_or_else(|| GenerateError::MissingResolvedParameter {
            peripheral: display.name().to_owned(),
            parameter: String::from("dc binding"),
        })?;
    let reset =
        display
            .binding("reset")
            .ok_or_else(|| GenerateError::MissingResolvedParameter {
                peripheral: display.name().to_owned(),
                parameter: String::from("reset binding"),
            })?;
    let backlight =
        display
            .binding("backlight")
            .ok_or_else(|| GenerateError::MissingResolvedParameter {
                peripheral: display.name().to_owned(),
                parameter: String::from("backlight binding"),
            })?;
    for identifier in [
        spi.peripheral(),
        spi.sck(),
        mosi,
        spi.chip_select(),
        dc,
        reset,
        backlight,
    ] {
        validate_hardware_identifier(identifier)?;
    }

    let controller = resolved_string(display, "controller")?;
    let model = match controller {
        "gc9a01" => "GC9A01",
        "gc9107" => "GC9107",
        "ili9342c" => "ILI9342CRgb565",
        "st7735s" => "ST7735s",
        "st7789" => "ST7789",
        _ => {
            return Err(GenerateError::UnsupportedResolvedValue {
                peripheral: display.name().to_owned(),
                parameter: String::from("controller"),
            });
        }
    };
    let width = resolved_dimension(display, "width")?;
    let height = resolved_dimension(display, "height")?;
    let offset_x = resolved_u16(display, "offset-x")?;
    let offset_y = resolved_u16(display, "offset-y")?;
    let color_order = match resolved_string(display, "color-order")? {
        "rgb" => "Rgb",
        "bgr" => "Bgr",
        _ => {
            return Err(GenerateError::UnsupportedResolvedValue {
                peripheral: display.name().to_owned(),
                parameter: String::from("color-order"),
            });
        }
    };
    let orientation = match resolved_string(display, "orientation")? {
        "deg0" => "Deg0",
        "deg90" => "Deg90",
        "deg180" => "Deg180",
        "deg270" => "Deg270",
        _ => {
            return Err(GenerateError::UnsupportedResolvedValue {
                peripheral: display.name().to_owned(),
                parameter: String::from("orientation"),
            });
        }
    };
    let invert = resolved_boolean(display, "invert-colors")?;
    let backlight_active_high = resolved_boolean(display, "backlight-active-high")?;
    let implementation = display.driver().implementation();
    let chip_crate = format!("barracuda_chip_{chip}");
    let factory = format!(
        "::{}::{}<::{chip_crate}::SpiDevice, ::{chip_crate}::DigitalOutput, ::{}::{model}, ::{chip_crate}::DigitalOutput, ::{chip_crate}::DigitalOutput, ::{chip_crate}::DriverDelay, 512>",
        implementation.crate_name(),
        implementation.driver_type(),
        implementation.crate_name(),
    );
    let capability = format!("<{factory} as PeripheralDriver>::Capability");
    let driver_error = format!("<{factory} as PeripheralDriver>::Error");
    let field = rust_identifier(display.name());

    let source = format!(
        "#[cfg(target_arch = \"xtensa\")]\nmod generated_board_hal {{\n\
         use core::fmt;\n\
         use ::barracuda_board_hal::{{BoardHal, BoardHalInitResult, BoardHalResources, NoExposedIo, PeripheralDriver, display::{{BuiltinDisplay, DisplayOrientation, PixelFormat}}}};\n\
         use ::barracuda_mipi_dbi_display::{{MipiDbiColorOrder, MipiDbiDisplayBindings, MipiDbiDisplayConfig, MipiDbiDriverConfig, Size}};\n\
         use ::embassy_executor::Spawner;\n\n\
         pub struct GeneratedBoardBindings {{\n\
             {field}_spi: ::{chip_crate}::peripherals::{spi_peripheral}<'static>,\n\
             {field}_sck: ::{chip_crate}::peripherals::{sck}<'static>,\n\
             {field}_mosi: ::{chip_crate}::peripherals::{mosi}<'static>,\n\
             {field}_chip_select: ::{chip_crate}::peripherals::{chip_select}<'static>,\n\
             {field}_dc: ::{chip_crate}::peripherals::{dc}<'static>,\n\
             {field}_reset: ::{chip_crate}::peripherals::{reset}<'static>,\n\
             {field}_backlight: ::{chip_crate}::peripherals::{backlight}<'static>,\n\
         }}\n\n\
         impl GeneratedBoardBindings {{\n\
             #[must_use]\n\
             pub const fn new(\n\
                 {field}_spi: ::{chip_crate}::peripherals::{spi_peripheral}<'static>,\n\
                 {field}_sck: ::{chip_crate}::peripherals::{sck}<'static>,\n\
                 {field}_mosi: ::{chip_crate}::peripherals::{mosi}<'static>,\n\
                 {field}_chip_select: ::{chip_crate}::peripherals::{chip_select}<'static>,\n\
                 {field}_dc: ::{chip_crate}::peripherals::{dc}<'static>,\n\
                 {field}_reset: ::{chip_crate}::peripherals::{reset}<'static>,\n\
                 {field}_backlight: ::{chip_crate}::peripherals::{backlight}<'static>,\n\
             ) -> Self {{\n\
                 Self {{ {field}_spi, {field}_sck, {field}_mosi, {field}_chip_select, {field}_dc, {field}_reset, {field}_backlight }}\n\
             }}\n\
         }}\n\n\
         type DisplayFactory = {factory};\n\
         pub type DisplayCapability = {capability};\n\n\
         pub struct GeneratedBuiltins {{ {field}: Option<DisplayCapability> }}\n\n\
         impl BuiltinDisplay for GeneratedBuiltins {{\n\
             type Display = DisplayCapability;\n\
             fn take_display(&mut self) -> Option<Self::Display> {{ self.{field}.take() }}\n\
         }}\n\n\
         #[derive(Debug)]\n\
         pub enum GeneratedBoardError {{\n\
             SpiConfig(::{chip_crate}::SpiConfigError),\n\
             Display({driver_error}),\n\
         }}\n\n\
         impl fmt::Display for GeneratedBoardError {{\n\
             fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {{\n\
                 match self {{\n\
                     Self::SpiConfig(_) => formatter.write_str(\"SPI configuration failed\"),\n\
                     Self::Display(_) => formatter.write_str(\"display initialization failed\"),\n\
                 }}\n\
             }}\n\
         }}\n\n\
         impl core::error::Error for GeneratedBoardError {{}}\n\n\
         pub struct SelectedBoardHal;\n\n\
         impl BoardHal for SelectedBoardHal {{\n\
             type Bindings = GeneratedBoardBindings;\n\
             type Resources = BoardHalResources<GeneratedBuiltins, NoExposedIo>;\n\
             type Error = GeneratedBoardError;\n\n\
             async fn initialize(_spawner: Spawner, bindings: Self::Bindings) -> BoardHalInitResult<Self> {{\n\
                 let spi = ::{chip_crate}::spi_device(\n\
                     bindings.{field}_spi, bindings.{field}_sck, bindings.{field}_mosi,\n\
                     bindings.{field}_chip_select, {frequency},\n\
                 ).map_err(GeneratedBoardError::SpiConfig)?;\n\
                 let config = MipiDbiDisplayConfig::new(\n\
                     Size::new({width}, {height}), PixelFormat::Rgb565, DisplayOrientation::{orientation},\n\
                 ).with_offset({offset_x}, {offset_y}).with_color_order(MipiDbiColorOrder::{color_order}).with_inverted_colors({invert}).with_digital_backlight({backlight_active_high});\n\
                 let {field} = <DisplayFactory as PeripheralDriver>::initialize(\n\
                     MipiDbiDisplayBindings::new(\n\
                         spi,\n\
                         ::{chip_crate}::digital_output(bindings.{field}_dc, ::barracuda_board_hal::DigitalLevel::Low),\n\
                         ::{chip_crate}::digital_output(bindings.{field}_reset, ::barracuda_board_hal::DigitalLevel::High),\n\
                         Some(::{chip_crate}::digital_output(bindings.{field}_backlight, ::barracuda_board_hal::DigitalLevel::Low)),\n\
                         ::{chip_crate}::DriverDelay::new(),\n\
                         ::{chip_crate}::take_primary_display_buffer(),\n\
                     ),\n\
                     MipiDbiDriverConfig::new(::{driver_crate}::{model}, config),\n\
                 ).await.map_err(GeneratedBoardError::Display)?;\n\
                 Ok(BoardHalResources::new(GeneratedBuiltins {{ {field}: Some({field}) }}, NoExposedIo))\n\
             }}\n\
         }}\n\
         }}\n\n\
         #[cfg(target_arch = \"xtensa\")]\n\
         pub use generated_board_hal::*;\n",
        spi_peripheral = spi.peripheral(),
        sck = spi.sck(),
        chip_select = spi.chip_select(),
        backlight = backlight,
        frequency = spi.frequency_hz(),
        driver_crate = implementation.crate_name(),
    );
    Ok(source)
}

fn render_esp_gdeh0154d67_hal(
    board: &BoardDefinition,
    display: &ResolvedPeripheral<'_>,
    chip: &str,
) -> Result<String, GenerateError> {
    let binding = |role: &str| {
        display
            .binding(role)
            .ok_or_else(|| GenerateError::MissingResolvedParameter {
                peripheral: display.name().to_owned(),
                parameter: format!("{role} binding"),
            })
    };
    let spi_name = binding("spi")?;
    let spi = board.internal_io().spi_device(spi_name).ok_or_else(|| {
        GenerateError::UnsupportedResolvedValue {
            peripheral: display.name().to_owned(),
            parameter: String::from("spi binding"),
        }
    })?;
    let mosi = spi
        .mosi()
        .ok_or_else(|| GenerateError::UnsupportedResolvedValue {
            peripheral: display.name().to_owned(),
            parameter: String::from("spi.mosi"),
        })?;
    if spi.miso().is_some() {
        return Err(GenerateError::UnsupportedResolvedValue {
            peripheral: display.name().to_owned(),
            parameter: String::from("spi.miso"),
        });
    }
    let busy = binding("busy")?;
    let dc = binding("dc")?;
    let reset = binding("reset")?;
    let power_hold = binding("power-hold")?;
    for identifier in [
        spi.peripheral(),
        spi.sck(),
        mosi,
        spi.chip_select(),
        busy,
        dc,
        reset,
        power_hold,
    ] {
        validate_hardware_identifier(identifier)?;
    }

    let implementation = display.driver().implementation();
    let chip_crate = format!("barracuda_chip_{chip}");
    let factory = format!(
        "::{}::{}<::{chip_crate}::SpiDevice, ::{chip_crate}::DigitalInput, ::{chip_crate}::DigitalOutput, ::{chip_crate}::DigitalOutput, ::{chip_crate}::DigitalOutput, ::{chip_crate}::DriverDelay>",
        implementation.crate_name(),
        implementation.driver_type(),
    );
    let capability = format!("<{factory} as PeripheralDriver>::Capability");
    let driver_error = format!("<{factory} as PeripheralDriver>::Error");
    let driver_crate = implementation.crate_name();
    let field = rust_identifier(display.name());

    Ok(format!(
        "#[cfg(target_arch = \"xtensa\")]\nmod generated_board_hal {{\n\
         use core::fmt;\n\
         use ::barracuda_board_hal::{{BoardHal, BoardHalInitResult, BoardHalResources, NoExposedIo, PeripheralDriver, display::BuiltinDisplay}};\n\
         use ::{driver_crate}::Gdeh0154d67Bindings;\n\
         use ::embassy_executor::Spawner;\n\n\
         pub struct GeneratedBoardBindings {{\n\
             {field}_spi: ::{chip_crate}::peripherals::{spi_peripheral}<'static>,\n\
             {field}_sck: ::{chip_crate}::peripherals::{sck}<'static>,\n\
             {field}_mosi: ::{chip_crate}::peripherals::{mosi}<'static>,\n\
             {field}_chip_select: ::{chip_crate}::peripherals::{chip_select}<'static>,\n\
             {field}_busy: ::{chip_crate}::peripherals::{busy}<'static>,\n\
             {field}_dc: ::{chip_crate}::peripherals::{dc}<'static>,\n\
             {field}_reset: ::{chip_crate}::peripherals::{reset}<'static>,\n\
             {field}_power_hold: ::{chip_crate}::peripherals::{power_hold}<'static>,\n\
         }}\n\n\
         impl GeneratedBoardBindings {{\n\
             #[must_use]\n\
             pub const fn new(\n\
                 {field}_spi: ::{chip_crate}::peripherals::{spi_peripheral}<'static>,\n\
                 {field}_sck: ::{chip_crate}::peripherals::{sck}<'static>,\n\
                 {field}_mosi: ::{chip_crate}::peripherals::{mosi}<'static>,\n\
                 {field}_chip_select: ::{chip_crate}::peripherals::{chip_select}<'static>,\n\
                 {field}_busy: ::{chip_crate}::peripherals::{busy}<'static>,\n\
                 {field}_dc: ::{chip_crate}::peripherals::{dc}<'static>,\n\
                 {field}_reset: ::{chip_crate}::peripherals::{reset}<'static>,\n\
                 {field}_power_hold: ::{chip_crate}::peripherals::{power_hold}<'static>,\n\
             ) -> Self {{\n\
                 Self {{ {field}_spi, {field}_sck, {field}_mosi, {field}_chip_select, {field}_busy, {field}_dc, {field}_reset, {field}_power_hold }}\n\
             }}\n\
         }}\n\n\
         type DisplayFactory = {factory};\n\
         pub type DisplayCapability = {capability};\n\n\
         pub struct GeneratedBuiltins {{ {field}: Option<DisplayCapability> }}\n\n\
         impl BuiltinDisplay for GeneratedBuiltins {{\n\
             type Display = DisplayCapability;\n\
             fn take_display(&mut self) -> Option<Self::Display> {{ self.{field}.take() }}\n\
         }}\n\n\
         #[derive(Debug)]\n\
         pub enum GeneratedBoardError {{\n\
             SpiConfig(::{chip_crate}::SpiConfigError),\n\
             Display({driver_error}),\n\
         }}\n\n\
         impl fmt::Display for GeneratedBoardError {{\n\
             fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {{\n\
                 match self {{\n\
                     Self::SpiConfig(_) => formatter.write_str(\"SPI configuration failed\"),\n\
                     Self::Display(_) => formatter.write_str(\"e-paper initialization failed\"),\n\
                 }}\n\
             }}\n\
         }}\n\n\
         impl core::error::Error for GeneratedBoardError {{}}\n\n\
         pub struct SelectedBoardHal;\n\n\
         impl BoardHal for SelectedBoardHal {{\n\
             type Bindings = GeneratedBoardBindings;\n\
             type Resources = BoardHalResources<GeneratedBuiltins, NoExposedIo>;\n\
             type Error = GeneratedBoardError;\n\n\
             async fn initialize(_spawner: Spawner, bindings: Self::Bindings) -> BoardHalInitResult<Self> {{\n\
                 let spi = ::{chip_crate}::spi_device(\n\
                     bindings.{field}_spi, bindings.{field}_sck, bindings.{field}_mosi,\n\
                     bindings.{field}_chip_select, {frequency},\n\
                 ).map_err(GeneratedBoardError::SpiConfig)?;\n\
                 let {field} = <DisplayFactory as PeripheralDriver>::initialize(\n\
                     Gdeh0154d67Bindings::new(\n\
                         spi,\n\
                         ::{chip_crate}::digital_input(bindings.{field}_busy),\n\
                         ::{chip_crate}::digital_output(bindings.{field}_dc, ::barracuda_board_hal::DigitalLevel::Low),\n\
                         ::{chip_crate}::digital_output(bindings.{field}_reset, ::barracuda_board_hal::DigitalLevel::High),\n\
                         ::{chip_crate}::digital_output(bindings.{field}_power_hold, ::barracuda_board_hal::DigitalLevel::Low),\n\
                         ::{chip_crate}::DriverDelay::new(),\n\
                     ),\n\
                     (),\n\
                 ).await.map_err(GeneratedBoardError::Display)?;\n\
                 Ok(BoardHalResources::new(GeneratedBuiltins {{ {field}: Some({field}) }}, NoExposedIo))\n\
             }}\n\
         }}\n\
         }}\n\n\
         #[cfg(target_arch = \"xtensa\")]\n\
         pub use generated_board_hal::*;\n",
        spi_peripheral = spi.peripheral(),
        sck = spi.sck(),
        chip_select = spi.chip_select(),
        frequency = spi.frequency_hz(),
    ))
}

fn resolved_string<'a>(
    peripheral: &'a ResolvedPeripheral<'_>,
    parameter: &str,
) -> Result<&'a str, GenerateError> {
    match peripheral.parameter(parameter) {
        Some(PeripheralParameter::String(value)) => Ok(value),
        _ => Err(GenerateError::MissingResolvedParameter {
            peripheral: peripheral.name().to_owned(),
            parameter: parameter.to_owned(),
        }),
    }
}

fn resolved_boolean(
    peripheral: &ResolvedPeripheral<'_>,
    parameter: &str,
) -> Result<bool, GenerateError> {
    match peripheral.parameter(parameter) {
        Some(PeripheralParameter::Boolean(value)) => Ok(*value),
        _ => Err(GenerateError::MissingResolvedParameter {
            peripheral: peripheral.name().to_owned(),
            parameter: parameter.to_owned(),
        }),
    }
}

fn resolved_dimension(
    peripheral: &ResolvedPeripheral<'_>,
    parameter: &str,
) -> Result<u32, GenerateError> {
    match peripheral.parameter(parameter) {
        Some(PeripheralParameter::Integer(value)) => {
            u32::try_from(*value).map_err(|_| GenerateError::UnsupportedResolvedValue {
                peripheral: peripheral.name().to_owned(),
                parameter: parameter.to_owned(),
            })
        }
        _ => Err(GenerateError::MissingResolvedParameter {
            peripheral: peripheral.name().to_owned(),
            parameter: parameter.to_owned(),
        }),
    }
}

fn resolved_u16(
    peripheral: &ResolvedPeripheral<'_>,
    parameter: &str,
) -> Result<u16, GenerateError> {
    let value = resolved_dimension(peripheral, parameter)?;
    u16::try_from(value).map_err(|_| GenerateError::UnsupportedResolvedValue {
        peripheral: peripheral.name().to_owned(),
        parameter: parameter.to_owned(),
    })
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
    result
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
