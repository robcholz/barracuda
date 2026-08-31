//! Build-time Board YAML loading and validation.
//!
//! This crate runs on the build host. Runtime and firmware crates consume the
//! generated static description from `barracuda-board` instead of parsing YAML.

use serde::Deserialize;
use std::{
    collections::BTreeMap,
    fs, io,
    path::{Component, Path, PathBuf},
};

/// Workspace-relative file containing the persistently selected Board name.
pub const SELECTED_BOARD_PATH: &str = ".barracuda/selected-board";

/// Failure while reading or writing the persistent Board selection.
#[derive(Debug, thiserror::Error)]
pub enum SelectionError {
    /// The persisted or requested Board name is unsafe or malformed.
    #[error("invalid Board name `{name}`")]
    InvalidName {
        /// Invalid value.
        name: String,
    },
    /// Local selection state could not be read or written.
    #[error("failed to access Board selection at `{path}`: {source}")]
    Io {
        /// State path that failed.
        path: PathBuf,
        /// Underlying filesystem failure.
        #[source]
        source: io::Error,
    },
}

/// Reads the Board selected for subsequent workspace builds.
///
/// # Errors
///
/// Returns [`SelectionError`] when the state file cannot be read or contains
/// an invalid Board name. A missing state file means no Board has been selected.
pub fn read_selected_board(workspace_root: &Path) -> Result<Option<String>, SelectionError> {
    let path = workspace_root.join(SELECTED_BOARD_PATH);
    let selected = match fs::read_to_string(&path) {
        Ok(selected) => selected,
        Err(source) if source.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(source) => return Err(SelectionError::Io { path, source }),
    };
    let name = selected.trim_end_matches(['\r', '\n']);
    validate_board_name(name)?;
    Ok(Some(name.to_owned()))
}

/// Persists the Board used by subsequent workspace builds.
///
/// # Errors
///
/// Returns [`SelectionError`] when `name` is invalid or local state cannot be
/// created and written.
pub fn write_selected_board(workspace_root: &Path, name: &str) -> Result<(), SelectionError> {
    validate_board_name(name)?;
    let path = workspace_root.join(SELECTED_BOARD_PATH);
    let parent = path.parent().unwrap_or(workspace_root);
    fs::create_dir_all(parent).map_err(|source| SelectionError::Io {
        path: parent.to_owned(),
        source,
    })?;
    fs::write(&path, format!("{name}\n")).map_err(|source| SelectionError::Io { path, source })
}

/// Validates a Board name before it is used as a bundle-directory component.
///
/// # Errors
///
/// Returns [`SelectionError::InvalidName`] for empty names or values containing
/// characters other than ASCII letters, digits, `-`, and `_`.
pub fn validate_board_name(name: &str) -> Result<(), SelectionError> {
    if !name.is_empty()
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        Ok(())
    } else {
        Err(SelectionError::InvalidName {
            name: name.to_owned(),
        })
    }
}

/// Parsed, validated Board definition.
#[derive(Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct BoardDefinition {
    name: String,
    hardware: HardwareDefinition,
    #[serde(default)]
    toolchain: Option<ToolchainDefinition>,
    #[serde(rename = "native-layout")]
    native_layout: NativeLayoutDefinition,
    #[serde(default, rename = "exposed-io")]
    exposed_io: ExposedIoDefinition,
    #[serde(default, rename = "builtin-peripherals")]
    builtin_peripherals: BTreeMap<String, BuiltinPeripheralDefinition>,
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

    /// Returns the cross-compilation toolchain declaration, when the Board has one.
    ///
    /// A Board that must be cross-compiled declares the Rust target triple it is
    /// built for. Host Boards omit this and build for the build host's native
    /// target. The target is build policy, not a runtime hardware fact, so it is
    /// consumed by the build driver (for example CI) and never baked into the
    /// generated runtime Board value.
    #[must_use]
    pub const fn toolchain(&self) -> Option<&ToolchainDefinition> {
        self.toolchain.as_ref()
    }

    /// Returns the Board-bundled native physical layout.
    #[must_use]
    pub const fn native_layout(&self) -> &NativeLayoutDefinition {
        &self.native_layout
    }

    /// Returns the I/O capabilities explicitly exposed by this Board.
    #[must_use]
    pub const fn exposed_io(&self) -> &ExposedIoDefinition {
        &self.exposed_io
    }

    /// Finds a built-in peripheral declaration by its Board-level name.
    #[must_use]
    pub fn builtin_peripheral(&self, name: &str) -> Option<&BuiltinPeripheralDefinition> {
        self.builtin_peripherals.get(name)
    }

    /// Returns the number of built-in peripheral declarations.
    #[must_use]
    pub fn builtin_peripheral_count(&self) -> usize {
        self.builtin_peripherals.len()
    }

    /// Returns whether this Board declares any built-in or exposed hardware.
    #[must_use]
    pub fn has_hardware_surface(&self) -> bool {
        !self.builtin_peripherals.is_empty() || !self.exposed_io.is_empty()
    }

    fn validate(&self) -> Result<(), ConfigError> {
        if self.hardware.chip.trim().is_empty() {
            return Err(ConfigError::EmptyChip);
        }
        if let Some(toolchain) = &self.toolchain {
            if toolchain.target.trim().is_empty() {
                return Err(ConfigError::EmptyToolchainTarget);
            }
        }
        let artifact = self.native_layout.artifact.trim();
        if artifact.is_empty() {
            return Err(ConfigError::EmptyNativeLayoutArtifact);
        }
        let path = Path::new(artifact);
        if path.is_absolute()
            || path
                .components()
                .any(|component| !matches!(component, Component::Normal(_)))
        {
            return Err(ConfigError::InvalidNativeLayoutArtifact);
        }
        self.exposed_io.validate()?;
        for (name, peripheral) in &self.builtin_peripherals {
            validate_resource_name("builtin-peripherals", name)?;
            validate_identifier(name, "driver", &peripheral.driver)?;
            for (binding, identifier) in &peripheral.bindings {
                validate_resource_name("builtin binding", binding)?;
                validate_identifier(name, "binding", identifier)?;
            }
            for parameter in peripheral.parameters.keys() {
                validate_resource_name("builtin parameter", parameter)?;
            }
        }
        Ok(())
    }
}

/// I/O declarations that become visible outside built-in peripheral Drivers.
#[derive(Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ExposedIoDefinition {
    #[serde(default)]
    gpio: BTreeMap<String, GpioDefinition>,
    #[serde(default, rename = "analog-input")]
    analog_input: BTreeMap<String, AnalogChannelDefinition>,
    #[serde(default, rename = "analog-output")]
    analog_output: BTreeMap<String, AnalogChannelDefinition>,
    #[serde(default)]
    pwm: BTreeMap<String, PwmDefinition>,
    #[serde(default)]
    i2c: BTreeMap<String, I2cDefinition>,
    #[serde(default)]
    spi: BTreeMap<String, SpiDefinition>,
}

impl ExposedIoDefinition {
    /// Returns the total number of explicitly exposed I/O resources.
    #[must_use]
    pub fn len(&self) -> usize {
        self.gpio.len()
            + self.analog_input.len()
            + self.analog_output.len()
            + self.pwm.len()
            + self.i2c.len()
            + self.spi.len()
    }

    /// Returns whether no I/O resources are exposed.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Finds a dynamically configurable digital GPIO by its Board-level name.
    #[must_use]
    pub fn gpio(&self, name: &str) -> Option<&GpioDefinition> {
        self.gpio.get(name)
    }

    /// Finds an analog input channel by its Board-level name.
    #[must_use]
    pub fn analog_input(&self, name: &str) -> Option<&AnalogChannelDefinition> {
        self.analog_input.get(name)
    }

    /// Finds an analog output channel by its Board-level name.
    #[must_use]
    pub fn analog_output(&self, name: &str) -> Option<&AnalogChannelDefinition> {
        self.analog_output.get(name)
    }

    /// Finds a PWM output by its Board-level name.
    #[must_use]
    pub fn pwm(&self, name: &str) -> Option<&PwmDefinition> {
        self.pwm.get(name)
    }

    /// Finds an I2C controller by its Board-level name.
    #[must_use]
    pub fn i2c(&self, name: &str) -> Option<&I2cDefinition> {
        self.i2c.get(name)
    }

    /// Finds an SPI controller by its Board-level name.
    #[must_use]
    pub fn spi(&self, name: &str) -> Option<&SpiDefinition> {
        self.spi.get(name)
    }

    fn validate(&self) -> Result<(), ConfigError> {
        for (name, gpio) in &self.gpio {
            validate_resource_name("gpio", name)?;
            validate_identifier(name, "pin", &gpio.pin)?;
        }
        for (section, channels) in [
            ("analog-input", &self.analog_input),
            ("analog-output", &self.analog_output),
        ] {
            for (name, channel) in channels {
                validate_resource_name(section, name)?;
                validate_identifier(name, "peripheral", &channel.peripheral)?;
                validate_identifier(name, "pin", &channel.pin)?;
            }
        }
        for (name, pwm) in &self.pwm {
            validate_resource_name("pwm", name)?;
            validate_identifier(name, "peripheral", &pwm.peripheral)?;
            validate_identifier(name, "pin", &pwm.pin)?;
        }
        for (name, i2c) in &self.i2c {
            validate_resource_name("i2c", name)?;
            validate_identifier(name, "peripheral", &i2c.peripheral)?;
            validate_identifier(name, "scl", &i2c.scl)?;
            validate_identifier(name, "sda", &i2c.sda)?;
            validate_frequency(name, i2c.frequency_hz)?;
        }
        for (name, spi) in &self.spi {
            validate_resource_name("spi", name)?;
            validate_identifier(name, "peripheral", &spi.peripheral)?;
            validate_identifier(name, "sck", &spi.sck)?;
            if let Some(mosi) = &spi.mosi {
                validate_identifier(name, "mosi", mosi)?;
            }
            if let Some(miso) = &spi.miso {
                validate_identifier(name, "miso", miso)?;
            }
            validate_frequency(name, spi.frequency_hz)?;
        }
        Ok(())
    }
}

/// One exposed digital GPIO declaration.
#[derive(Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct GpioDefinition {
    pin: String,
}

impl GpioDefinition {
    /// Returns the chip-native pin identifier.
    #[must_use]
    pub fn pin(&self) -> &str {
        &self.pin
    }
}

/// One ADC or DAC channel declaration.
#[derive(Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AnalogChannelDefinition {
    peripheral: String,
    pin: String,
    channel: u8,
}

impl AnalogChannelDefinition {
    /// Returns the chip-native ADC or DAC peripheral identifier.
    #[must_use]
    pub fn peripheral(&self) -> &str {
        &self.peripheral
    }

    /// Returns the chip-native pin identifier.
    #[must_use]
    pub fn pin(&self) -> &str {
        &self.pin
    }

    /// Returns the peripheral channel number.
    #[must_use]
    pub const fn channel(&self) -> u8 {
        self.channel
    }
}

/// One PWM channel declaration.
#[derive(Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PwmDefinition {
    peripheral: String,
    pin: String,
    channel: u8,
}

impl PwmDefinition {
    /// Returns the chip-native PWM peripheral identifier.
    #[must_use]
    pub fn peripheral(&self) -> &str {
        &self.peripheral
    }

    /// Returns the chip-native pin identifier.
    #[must_use]
    pub fn pin(&self) -> &str {
        &self.pin
    }

    /// Returns the peripheral channel number.
    #[must_use]
    pub const fn channel(&self) -> u8 {
        self.channel
    }
}

/// One exposed I2C controller and its signal pins.
#[derive(Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct I2cDefinition {
    peripheral: String,
    scl: String,
    sda: String,
    #[serde(rename = "frequency-hz")]
    frequency_hz: u32,
}

impl I2cDefinition {
    /// Returns the chip-native I2C peripheral identifier.
    #[must_use]
    pub fn peripheral(&self) -> &str {
        &self.peripheral
    }

    /// Returns the chip-native clock pin identifier.
    #[must_use]
    pub fn scl(&self) -> &str {
        &self.scl
    }

    /// Returns the chip-native data pin identifier.
    #[must_use]
    pub fn sda(&self) -> &str {
        &self.sda
    }

    /// Returns the initial controller frequency.
    #[must_use]
    pub const fn frequency_hz(&self) -> u32 {
        self.frequency_hz
    }
}

/// One exposed SPI controller and its signal pins.
#[derive(Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SpiDefinition {
    peripheral: String,
    sck: String,
    #[serde(default)]
    mosi: Option<String>,
    #[serde(default)]
    miso: Option<String>,
    #[serde(rename = "frequency-hz")]
    frequency_hz: u32,
}

impl SpiDefinition {
    /// Returns the chip-native SPI peripheral identifier.
    #[must_use]
    pub fn peripheral(&self) -> &str {
        &self.peripheral
    }

    /// Returns the chip-native clock pin identifier.
    #[must_use]
    pub fn sck(&self) -> &str {
        &self.sck
    }

    /// Returns the optional controller-output pin identifier.
    #[must_use]
    pub fn mosi(&self) -> Option<&str> {
        self.mosi.as_deref()
    }

    /// Returns the optional controller-input pin identifier.
    #[must_use]
    pub fn miso(&self) -> Option<&str> {
        self.miso.as_deref()
    }

    /// Returns the initial controller frequency.
    #[must_use]
    pub const fn frequency_hz(&self) -> u32 {
        self.frequency_hz
    }
}

/// One fixed Board peripheral and the Driver bindings used to construct it.
#[derive(Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct BuiltinPeripheralDefinition {
    driver: String,
    #[serde(default)]
    bindings: BTreeMap<String, String>,
    #[serde(default)]
    parameters: BTreeMap<String, PeripheralParameter>,
}

impl BuiltinPeripheralDefinition {
    /// Returns the stable peripheral Driver identifier.
    #[must_use]
    pub fn driver(&self) -> &str {
        &self.driver
    }

    /// Finds one chip-native binding by its Driver-defined role.
    #[must_use]
    pub fn binding(&self, role: &str) -> Option<&str> {
        self.bindings.get(role).map(String::as_str)
    }

    /// Finds one Driver-defined construction parameter.
    #[must_use]
    pub fn parameter(&self, name: &str) -> Option<&PeripheralParameter> {
        self.parameters.get(name)
    }
}

/// Build-time value passed to a built-in peripheral Driver constructor.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(untagged)]
pub enum PeripheralParameter {
    /// Boolean setting.
    Boolean(bool),
    /// Signed integer setting.
    Integer(i64),
    /// Text setting or chip-native identifier interpreted by the Driver.
    String(String),
    /// Ordered collection of settings.
    Sequence(Vec<PeripheralParameter>),
    /// Named collection of nested settings.
    Mapping(BTreeMap<String, PeripheralParameter>),
}

fn validate_resource_name(section: &'static str, name: &str) -> Result<(), ConfigError> {
    if name.trim().is_empty() {
        Err(ConfigError::EmptyHardwareResourceName { section })
    } else {
        Ok(())
    }
}

fn validate_identifier(
    resource: &str,
    field: &'static str,
    identifier: &str,
) -> Result<(), ConfigError> {
    if identifier.trim().is_empty() {
        Err(ConfigError::EmptyHardwareIdentifier {
            resource: resource.to_owned(),
            field,
        })
    } else {
        Ok(())
    }
}

fn validate_frequency(resource: &str, frequency_hz: u32) -> Result<(), ConfigError> {
    if frequency_hz == 0 {
        Err(ConfigError::ZeroProtocolFrequency {
            resource: resource.to_owned(),
        })
    } else {
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

/// Toolchain policy a Board carries so the build driver can choose a target.
#[derive(Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ToolchainDefinition {
    target: String,
}

impl ToolchainDefinition {
    /// Returns the Rust target triple this Board is cross-compiled for.
    #[must_use]
    pub fn target(&self) -> &str {
        &self.target
    }
}

/// A Platform-native layout file stored inside the Board bundle.
#[derive(Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct NativeLayoutDefinition {
    artifact: String,
}

impl NativeLayoutDefinition {
    /// Returns the artifact path relative to the Board bundle.
    #[must_use]
    pub fn artifact(&self) -> &str {
        &self.artifact
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
    /// A Board must identify its concrete chip or native operating-system runtime.
    #[error("Board hardware chip must not be empty")]
    EmptyChip,
    /// A Board declared a toolchain section without a target triple.
    #[error("Board toolchain target must not be empty")]
    EmptyToolchainTarget,
    /// A Board did not identify its native physical-layout artifact.
    #[error("Board native-layout artifact must not be empty")]
    EmptyNativeLayoutArtifact,
    /// A native-layout artifact is absolute or escapes the Board bundle.
    #[error("Board native-layout artifact must remain inside its Board bundle")]
    InvalidNativeLayoutArtifact,
    /// A map contains an empty Board-level resource name.
    #[error("Board `{section}` resource name must not be empty")]
    EmptyHardwareResourceName {
        /// Section containing the invalid name.
        section: &'static str,
    },
    /// A declared resource contains an empty chip-native identifier.
    #[error("Board resource `{resource}` field `{field}` must not be empty")]
    EmptyHardwareIdentifier {
        /// Board-level resource name.
        resource: String,
        /// Field containing the invalid identifier.
        field: &'static str,
    },
    /// An I2C or SPI controller was configured with a zero frequency.
    #[error("Board protocol resource `{resource}` frequency must be greater than zero")]
    ZeroProtocolFrequency {
        /// Board-level resource name.
        resource: String,
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
    format!(
        "/// Board selected by the build configuration.\n\
         pub const BOARD: ::barracuda_board::Board = ::barracuda_board::Board::new(\n    {:?},\n    ::barracuda_board::Hardware::new({:?}),\n    ::barracuda_board::NativeLayout::new({:?}),\n);\n",
        board.name(),
        board.hardware().chip(),
        board.native_layout().artifact(),
    )
}
