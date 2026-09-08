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
    #[serde(default, rename = "internal-io")]
    internal_io: InternalIoDefinition,
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
    /// target. The target is build policy, not a runtime hardware fact, so Board
    /// selection writes it into local Cargo configuration rather than the
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

    /// Returns move-only I/O resources reserved for built-in peripheral Drivers.
    #[must_use]
    pub const fn internal_io(&self) -> &InternalIoDefinition {
        &self.internal_io
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

    /// Iterates built-in peripherals in stable Board-name order.
    pub fn builtin_peripherals(
        &self,
    ) -> impl Iterator<Item = (&str, &BuiltinPeripheralDefinition)> {
        self.builtin_peripherals
            .iter()
            .map(|(name, peripheral)| (name.as_str(), peripheral))
    }

    /// Returns whether this Board declares any built-in or exposed hardware.
    #[must_use]
    pub fn has_hardware_surface(&self) -> bool {
        !self.builtin_peripherals.is_empty()
            || !self.exposed_io.is_empty()
            || !self.internal_io.is_empty()
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
        self.internal_io.validate()?;
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

/// Named protocol resources consumed exclusively by built-in peripheral Drivers.
///
/// Internal resources are never returned through [`ExposedIoDefinition`]. A
/// built-in binds to one by its stable Board-local name, allowing chip-native
/// controller and pin tokens to remain in `board.yml`.
#[derive(Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct InternalIoDefinition {
    #[serde(default, rename = "spi-bus")]
    spi_buses: BTreeMap<String, SpiDefinition>,
    #[serde(default, rename = "spi-output")]
    spi_outputs: BTreeMap<String, SpiOutputDefinition>,
    #[serde(default, rename = "spi-device")]
    spi_devices: BTreeMap<String, SpiDeviceDefinition>,
    #[serde(default, rename = "i2c-device")]
    i2c_devices: BTreeMap<String, I2cDefinition>,
    #[serde(default, rename = "camera-capture")]
    camera_captures: BTreeMap<String, CameraCaptureDefinition>,
    #[serde(default, rename = "i2s-stream")]
    i2s_streams: BTreeMap<String, I2sStreamDefinition>,
}

impl InternalIoDefinition {
    /// Returns whether no internal resources are declared.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.spi_buses.is_empty()
            && self.spi_outputs.is_empty()
            && self.spi_devices.is_empty()
            && self.i2c_devices.is_empty()
            && self.camera_captures.is_empty()
            && self.i2s_streams.is_empty()
    }

    /// Finds one SPI bus reserved by a built-in Driver.
    #[must_use]
    pub fn spi_bus(&self, name: &str) -> Option<&SpiDefinition> {
        self.spi_buses.get(name)
    }

    /// Finds one data-only SPI waveform output by its Board-local name.
    #[must_use]
    pub fn spi_output(&self, name: &str) -> Option<&SpiOutputDefinition> {
        self.spi_outputs.get(name)
    }

    /// Finds one statically selected SPI device by its Board-local name.
    #[must_use]
    pub fn spi_device(&self, name: &str) -> Option<&SpiDeviceDefinition> {
        self.spi_devices.get(name)
    }

    /// Finds one statically selected I2C bus reserved by a built-in Driver.
    #[must_use]
    pub fn i2c_device(&self, name: &str) -> Option<&I2cDefinition> {
        self.i2c_devices.get(name)
    }

    /// Finds one parallel camera receiver reserved by a built-in Driver.
    #[must_use]
    pub fn camera_capture(&self, name: &str) -> Option<&CameraCaptureDefinition> {
        self.camera_captures.get(name)
    }

    /// Finds one I2S stream reserved by a built-in Driver.
    #[must_use]
    pub fn i2s_stream(&self, name: &str) -> Option<&I2sStreamDefinition> {
        self.i2s_streams.get(name)
    }

    /// Returns whether a fixed built-in declaration consumes this controller.
    ///
    /// Runtime I/O controller pools use this to exclude singleton tokens that
    /// are already assigned to statically composed Driver bindings.
    #[must_use]
    pub fn uses_controller(&self, peripheral: &str) -> bool {
        self.spi_buses
            .values()
            .any(|binding| binding.peripheral == peripheral)
            || self
                .spi_devices
                .values()
                .any(|binding| binding.peripheral == peripheral)
            || self
                .i2c_devices
                .values()
                .any(|binding| binding.peripheral == peripheral)
            || self
                .camera_captures
                .values()
                .any(|binding| binding.peripheral == peripheral)
            || self
                .i2s_streams
                .values()
                .any(|binding| binding.peripheral == peripheral)
    }

    /// Returns whether a fixed built-in declaration consumes this DMA channel.
    ///
    /// Platform-owned runtime DMA pools use this to remove channels already
    /// moved into camera or I2S Driver bindings.
    #[must_use]
    pub fn uses_dma(&self, dma: &str) -> bool {
        self.camera_captures
            .values()
            .any(|binding| binding.dma == dma)
            || self.i2s_streams.values().any(|binding| binding.dma == dma)
    }

    fn validate(&self) -> Result<(), ConfigError> {
        for (name, spi) in &self.spi_buses {
            validate_resource_name("internal spi-bus", name)?;
            validate_identifier(name, "peripheral", &spi.peripheral)?;
            validate_identifier(name, "sck", &spi.sck)?;
            if let Some(mosi) = &spi.mosi {
                validate_identifier(name, "mosi", mosi)?;
            }
            if let Some(miso) = &spi.miso {
                validate_identifier(name, "miso", miso)?;
            }
            validate_spi_data_pin(name, spi.mosi.as_deref(), spi.miso.as_deref())?;
            validate_frequency(name, spi.frequency_hz)?;
        }
        for (name, output) in &self.spi_outputs {
            validate_resource_name("internal spi-output", name)?;
            validate_identifier(name, "data", &output.data)?;
            validate_frequency(name, output.frequency_hz)?;
        }
        for (name, spi) in &self.spi_devices {
            validate_resource_name("internal spi-device", name)?;
            validate_identifier(name, "peripheral", &spi.peripheral)?;
            validate_identifier(name, "sck", &spi.sck)?;
            validate_identifier(name, "chip-select", &spi.chip_select)?;
            if let Some(mosi) = &spi.mosi {
                validate_identifier(name, "mosi", mosi)?;
            }
            if let Some(miso) = &spi.miso {
                validate_identifier(name, "miso", miso)?;
            }
            validate_spi_data_pin(name, spi.mosi.as_deref(), spi.miso.as_deref())?;
            validate_frequency(name, spi.frequency_hz)?;
        }
        for (name, i2c) in &self.i2c_devices {
            validate_resource_name("internal i2c-device", name)?;
            i2c.validate(name)?;
        }
        for (name, camera) in &self.camera_captures {
            validate_resource_name("internal camera-capture", name)?;
            camera.validate(name)?;
        }
        for (name, stream) in &self.i2s_streams {
            validate_resource_name("internal i2s-stream", name)?;
            stream.validate(name)?;
        }
        Ok(())
    }
}

/// One parallel camera receiver and its fixed Board wiring.
#[derive(Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CameraCaptureDefinition {
    peripheral: String,
    dma: String,
    xclk: String,
    pclk: String,
    vsync: String,
    href: String,
    data: [String; 8],
    #[serde(rename = "xclk-frequency-hz")]
    xclk_frequency_hz: u32,
    #[serde(rename = "dma-buffer-bytes")]
    dma_buffer_bytes: usize,
}

impl CameraCaptureDefinition {
    /// Returns the chip-native camera controller.
    #[must_use]
    pub fn peripheral(&self) -> &str {
        &self.peripheral
    }
    /// Returns the chip-native DMA channel.
    #[must_use]
    pub fn dma(&self) -> &str {
        &self.dma
    }
    /// Returns the sensor master-clock pin.
    #[must_use]
    pub fn xclk(&self) -> &str {
        &self.xclk
    }
    /// Returns the sensor pixel-clock pin.
    #[must_use]
    pub fn pclk(&self) -> &str {
        &self.pclk
    }
    /// Returns the vertical-sync pin.
    #[must_use]
    pub fn vsync(&self) -> &str {
        &self.vsync
    }
    /// Returns the horizontal-reference pin.
    #[must_use]
    pub fn href(&self) -> &str {
        &self.href
    }
    /// Returns the eight parallel data pins in least-significant-bit order.
    #[must_use]
    pub fn data(&self) -> &[String; 8] {
        &self.data
    }
    /// Returns the sensor master-clock frequency.
    #[must_use]
    pub const fn xclk_frequency_hz(&self) -> u32 {
        self.xclk_frequency_hz
    }
    /// Returns the statically allocated DMA frame-buffer capacity.
    #[must_use]
    pub const fn dma_buffer_bytes(&self) -> usize {
        self.dma_buffer_bytes
    }

    fn validate(&self, name: &str) -> Result<(), ConfigError> {
        for (field, value) in [
            ("peripheral", self.peripheral.as_str()),
            ("dma", self.dma.as_str()),
            ("xclk", self.xclk.as_str()),
            ("pclk", self.pclk.as_str()),
            ("vsync", self.vsync.as_str()),
            ("href", self.href.as_str()),
        ] {
            validate_identifier(name, field, value)?;
        }
        for pin in &self.data {
            validate_identifier(name, "data", pin)?;
        }
        validate_frequency(name, self.xclk_frequency_hz)?;
        if self.dma_buffer_bytes == 0 {
            return Err(ConfigError::ZeroDmaBuffer {
                resource: name.to_owned(),
            });
        }
        Ok(())
    }
}

/// One full-duplex I2S controller and its fixed Board wiring.
#[derive(Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct I2sStreamDefinition {
    peripheral: String,
    dma: String,
    bclk: String,
    ws: String,
    dout: String,
    din: String,
    #[serde(default)]
    mclk: Option<String>,
    #[serde(rename = "sample-rate-hz")]
    sample_rate_hz: u32,
    channels: u8,
    #[serde(rename = "bits-per-sample")]
    bits_per_sample: u8,
    #[serde(rename = "dma-buffer-bytes")]
    dma_buffer_bytes: usize,
}

impl I2sStreamDefinition {
    /// Returns the chip-native I2S controller.
    #[must_use]
    pub fn peripheral(&self) -> &str {
        &self.peripheral
    }
    /// Returns the chip-native DMA channel.
    #[must_use]
    pub fn dma(&self) -> &str {
        &self.dma
    }
    /// Returns the bit-clock pin.
    #[must_use]
    pub fn bclk(&self) -> &str {
        &self.bclk
    }
    /// Returns the word-select pin.
    #[must_use]
    pub fn ws(&self) -> &str {
        &self.ws
    }
    /// Returns the controller-to-codec data pin.
    #[must_use]
    pub fn dout(&self) -> &str {
        &self.dout
    }
    /// Returns the codec-to-controller data pin.
    #[must_use]
    pub fn din(&self) -> &str {
        &self.din
    }
    /// Returns the optional master-clock pin.
    #[must_use]
    pub fn mclk(&self) -> Option<&str> {
        self.mclk.as_deref()
    }
    /// Returns the sample rate.
    #[must_use]
    pub const fn sample_rate_hz(&self) -> u32 {
        self.sample_rate_hz
    }
    /// Returns the channel count.
    #[must_use]
    pub const fn channels(&self) -> u8 {
        self.channels
    }
    /// Returns the valid PCM bits in each word.
    #[must_use]
    pub const fn bits_per_sample(&self) -> u8 {
        self.bits_per_sample
    }
    /// Returns each statically allocated I2S DMA buffer's capacity.
    #[must_use]
    pub const fn dma_buffer_bytes(&self) -> usize {
        self.dma_buffer_bytes
    }

    fn validate(&self, name: &str) -> Result<(), ConfigError> {
        for (field, value) in [
            ("peripheral", self.peripheral.as_str()),
            ("dma", self.dma.as_str()),
            ("bclk", self.bclk.as_str()),
            ("ws", self.ws.as_str()),
            ("dout", self.dout.as_str()),
            ("din", self.din.as_str()),
        ] {
            validate_identifier(name, field, value)?;
        }
        if let Some(mclk) = &self.mclk {
            validate_identifier(name, "mclk", mclk)?;
        }
        validate_frequency(name, self.sample_rate_hz)?;
        if self.channels == 0 || self.bits_per_sample == 0 {
            return Err(ConfigError::InvalidI2sFormat {
                resource: name.to_owned(),
            });
        }
        if self.dma_buffer_bytes == 0 {
            return Err(ConfigError::ZeroDmaBuffer {
                resource: name.to_owned(),
            });
        }
        Ok(())
    }
}

/// I/O declarations that become visible outside built-in peripheral Drivers.
#[derive(Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ExposedIoDefinition {
    #[serde(default)]
    pins: BTreeMap<String, PinDefinition>,
}

impl ExposedIoDefinition {
    /// Returns the total number of explicitly exposed I/O resources.
    #[must_use]
    pub fn len(&self) -> usize {
        self.pins.len()
    }

    /// Returns whether no I/O resources are exposed.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Finds one multifunction physical pin by its Board-level name.
    #[must_use]
    pub fn pin(&self, name: &str) -> Option<&PinDefinition> {
        self.pins.get(name)
    }

    /// Iterates exposed physical pins in stable Board-name order.
    pub fn pins(&self) -> impl Iterator<Item = (&str, &PinDefinition)> {
        self.pins.iter().map(|(name, pin)| (name.as_str(), pin))
    }

    fn validate(&self) -> Result<(), ConfigError> {
        for (name, pin) in &self.pins {
            validate_resource_name("pin", name)?;
            validate_identifier(name, "pin", &pin.pin)?;
        }
        Ok(())
    }
}

/// One exposed multifunction physical pin.
#[derive(Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PinDefinition {
    pin: String,
}

impl PinDefinition {
    /// Returns the chip-native pin identifier.
    #[must_use]
    pub fn pin(&self) -> &str {
        &self.pin
    }
}

/// One statically wired I2C controller used by a built-in Driver.
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

    fn validate(&self, name: &str) -> Result<(), ConfigError> {
        validate_identifier(name, "peripheral", &self.peripheral)?;
        validate_identifier(name, "scl", &self.scl)?;
        validate_identifier(name, "sda", &self.sda)?;
        validate_frequency(name, self.frequency_hz)
    }
}

/// One statically wired SPI controller used by a built-in Driver.
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

/// One data-only SPI waveform output whose controller is Platform-selected.
#[derive(Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SpiOutputDefinition {
    data: String,
    #[serde(rename = "frequency-hz")]
    frequency_hz: u32,
}

impl SpiOutputDefinition {
    /// Returns the physical waveform output pin.
    #[must_use]
    pub fn data(&self) -> &str {
        &self.data
    }

    /// Returns the SPI clock rate used to synthesize the waveform.
    #[must_use]
    pub const fn frequency_hz(&self) -> u32 {
        self.frequency_hz
    }
}

/// One internal SPI device, including the chip-select owned by its Driver.
#[derive(Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SpiDeviceDefinition {
    peripheral: String,
    sck: String,
    #[serde(default)]
    mosi: Option<String>,
    #[serde(default)]
    miso: Option<String>,
    #[serde(rename = "chip-select")]
    chip_select: String,
    #[serde(rename = "frequency-hz")]
    frequency_hz: u32,
}

impl SpiDeviceDefinition {
    /// Returns the chip-native SPI controller identifier.
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

    /// Returns the chip-select pin owned by this selected device.
    #[must_use]
    pub fn chip_select(&self) -> &str {
        &self.chip_select
    }

    /// Returns the initial bus frequency for this device.
    #[must_use]
    pub const fn frequency_hz(&self) -> u32 {
        self.frequency_hz
    }
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

    /// Iterates chip-native bindings in stable role-name order.
    pub fn bindings(&self) -> impl Iterator<Item = (&str, &str)> {
        self.bindings
            .iter()
            .map(|(role, identifier)| (role.as_str(), identifier.as_str()))
    }

    /// Finds one Driver-defined construction parameter.
    #[must_use]
    pub fn parameter(&self, name: &str) -> Option<&PeripheralParameter> {
        self.parameters.get(name)
    }

    /// Iterates Driver parameters in stable name order.
    pub fn parameters(&self) -> impl Iterator<Item = (&str, &PeripheralParameter)> {
        self.parameters
            .iter()
            .map(|(name, value)| (name.as_str(), value))
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

fn validate_spi_data_pin(
    resource: &str,
    mosi: Option<&str>,
    miso: Option<&str>,
) -> Result<(), ConfigError> {
    if mosi.is_none() && miso.is_none() {
        Err(ConfigError::MissingSpiDataPin {
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

/// Toolchain policy a Board carries so selection can configure Cargo's target.
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
    /// An SPI declaration has neither a controller-output nor controller-input pin.
    #[error("Board SPI resource `{resource}` must declare `mosi`, `miso`, or both")]
    MissingSpiDataPin {
        /// Board-level SPI resource name.
        resource: String,
    },
    /// An I2S stream has no channels or no valid sample bits.
    #[error("Board I2S stream `{resource}` must have channels and sample bits")]
    InvalidI2sFormat {
        /// Board-level I2S resource name.
        resource: String,
    },
    /// A DMA-backed media resource requested no storage.
    #[error("Board DMA resource `{resource}` buffer must be greater than zero bytes")]
    ZeroDmaBuffer {
        /// Board-level camera or I2S resource name.
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
