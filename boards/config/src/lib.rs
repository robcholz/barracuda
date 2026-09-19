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
    #[serde(default)]
    peripherals: PeripheralsDefinition,
    #[serde(default, rename = "exposed-io")]
    exposed_io: ExposedIoDefinition,
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

    /// Returns the Board's optional attached peripherals and their private I/O.
    #[must_use]
    pub const fn peripherals(&self) -> &PeripheralsDefinition {
        &self.peripherals
    }

    /// Returns the I/O entries explicitly exposed by this Board.
    #[must_use]
    pub const fn exposed_io(&self) -> &ExposedIoDefinition {
        &self.exposed_io
    }

    /// Returns the fixed I/O resources consumed by attached peripheral implementations.
    #[must_use]
    pub const fn peripheral_io(&self) -> &PeripheralIoDefinition {
        &self.peripherals.io
    }

    /// Finds an attached peripheral declaration by its Board-level name.
    #[must_use]
    pub fn peripheral(&self, name: &str) -> Option<&PeripheralDefinition> {
        self.peripherals.devices.get(name)
    }

    /// Returns the number of attached peripheral declarations.
    #[must_use]
    pub fn peripheral_count(&self) -> usize {
        self.peripherals.devices.len()
    }

    /// Iterates attached peripherals in stable Board-name order.
    pub fn peripheral_devices(&self) -> impl Iterator<Item = (&str, &PeripheralDefinition)> {
        self.peripherals
            .devices
            .iter()
            .map(|(name, peripheral)| (name.as_str(), peripheral))
    }

    /// Returns whether this Board declares any attached peripherals or exposed I/O.
    #[must_use]
    pub fn has_hardware_surface(&self) -> bool {
        !self.peripherals.is_empty() || !self.exposed_io.is_empty()
    }

    fn validate(&self) -> Result<(), ConfigError> {
        if self.hardware.chip.trim().is_empty() {
            return Err(ConfigError::EmptyChip);
        }
        if self
            .hardware
            .external_memory
            .as_ref()
            .is_some_and(|memory| memory.size_bytes == 0)
        {
            return Err(ConfigError::ZeroExternalMemory);
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
        self.peripherals.io.validate()?;
        self.exposed_io.validate()?;
        for (name, peripheral) in &self.peripherals.devices {
            validate_resource_name("peripherals", name)?;
            validate_identifier(name, "implementation", &peripheral.implementation)?;
            for (binding, identifier) in &peripheral.bindings {
                validate_resource_name("peripheral binding", binding)?;
                validate_identifier(name, "binding", identifier)?;
            }
            for parameter in peripheral.parameters.keys() {
                validate_resource_name("peripheral parameter", parameter)?;
            }
        }
        Ok(())
    }
}

/// Attached peripherals and the private I/O resources used to construct them.
#[derive(Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PeripheralsDefinition {
    #[serde(default)]
    io: PeripheralIoDefinition,
    #[serde(default)]
    devices: BTreeMap<String, PeripheralDefinition>,
}

impl PeripheralsDefinition {
    /// Returns private I/O resources consumed by peripheral implementations.
    #[must_use]
    pub const fn io(&self) -> &PeripheralIoDefinition {
        &self.io
    }

    /// Iterates optional attached peripherals in stable Board-name order.
    pub fn devices(&self) -> impl Iterator<Item = (&str, &PeripheralDefinition)> {
        self.devices
            .iter()
            .map(|(name, peripheral)| (name.as_str(), peripheral))
    }

    /// Returns whether neither peripheral devices nor private I/O are declared.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.io.is_empty() && self.devices.is_empty()
    }
}

/// Named protocol resources consumed exclusively by attached peripheral implementations.
///
/// Internal resources are never returned through [`ExposedIoDefinition`]. A
/// peripheral binds to one by its stable Board-local name, allowing chip-native
/// controller and pin tokens to remain in `board.yml`.
#[derive(Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PeripheralIoDefinition {
    #[serde(default, rename = "dsi-host")]
    dsi_hosts: BTreeMap<String, DsiHostDefinition>,
    #[serde(default, rename = "mipi-csi")]
    mipi_csi_hosts: BTreeMap<String, MipiCsiDefinition>,
    #[serde(default, rename = "sdmmc-device")]
    sdmmc_devices: BTreeMap<String, SdmmcDeviceDefinition>,
    #[serde(default, rename = "spi-bus")]
    spi_buses: BTreeMap<String, SpiDefinition>,
    #[serde(default, rename = "spi-output")]
    spi_outputs: BTreeMap<String, SpiOutputDefinition>,
    #[serde(default, rename = "spi-device")]
    spi_devices: BTreeMap<String, SpiDeviceDefinition>,
    #[serde(default, rename = "quad-spi-device")]
    quad_spi_devices: BTreeMap<String, QuadSpiDeviceDefinition>,
    #[serde(default, rename = "i2c-device")]
    i2c_devices: BTreeMap<String, I2cDefinition>,
    #[serde(default, rename = "camera-capture")]
    camera_captures: BTreeMap<String, CameraCaptureDefinition>,
    #[serde(default, rename = "i2s-stream")]
    i2s_streams: BTreeMap<String, I2sStreamDefinition>,
    #[serde(default, rename = "capacitive-touch")]
    capacitive_touch: BTreeMap<String, CapacitiveTouchDefinition>,
}

impl PeripheralIoDefinition {
    /// Returns whether no internal resources are declared.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.dsi_hosts.is_empty()
            && self.mipi_csi_hosts.is_empty()
            && self.sdmmc_devices.is_empty()
            && self.spi_buses.is_empty()
            && self.spi_outputs.is_empty()
            && self.spi_devices.is_empty()
            && self.quad_spi_devices.is_empty()
            && self.i2c_devices.is_empty()
            && self.camera_captures.is_empty()
            && self.i2s_streams.is_empty()
            && self.capacitive_touch.is_empty()
    }

    /// Finds one MIPI DSI host reserved by a display implementation.
    #[must_use]
    pub fn dsi_host(&self, name: &str) -> Option<&DsiHostDefinition> {
        self.dsi_hosts.get(name)
    }

    /// Finds one MIPI CSI host reserved by a camera implementation.
    #[must_use]
    pub fn mipi_csi(&self, name: &str) -> Option<&MipiCsiDefinition> {
        self.mipi_csi_hosts.get(name)
    }

    /// Finds one SDMMC device reserved by a storage implementation.
    #[must_use]
    pub fn sdmmc_device(&self, name: &str) -> Option<&SdmmcDeviceDefinition> {
        self.sdmmc_devices.get(name)
    }

    /// Finds one SPI bus reserved by a peripheral implementation.
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

    /// Finds one statically selected four-data-line SPI device.
    #[must_use]
    pub fn quad_spi_device(&self, name: &str) -> Option<&QuadSpiDeviceDefinition> {
        self.quad_spi_devices.get(name)
    }

    /// Finds one statically selected I2C bus reserved by a peripheral implementation.
    #[must_use]
    pub fn i2c_device(&self, name: &str) -> Option<&I2cDefinition> {
        self.i2c_devices.get(name)
    }

    /// Finds one parallel camera receiver reserved by a peripheral implementation.
    #[must_use]
    pub fn camera_capture(&self, name: &str) -> Option<&CameraCaptureDefinition> {
        self.camera_captures.get(name)
    }

    /// Finds one I2S stream reserved by a peripheral implementation.
    #[must_use]
    pub fn i2s_stream(&self, name: &str) -> Option<&I2sStreamDefinition> {
        self.i2s_streams.get(name)
    }

    /// Finds one fixed capacitive-touch channel group.
    #[must_use]
    pub fn capacitive_touch(&self, name: &str) -> Option<&CapacitiveTouchDefinition> {
        self.capacitive_touch.get(name)
    }

    /// Returns whether a fixed built-in declaration consumes this controller.
    ///
    /// Runtime I/O controller pools use this to exclude singleton tokens that
    /// are already assigned to statically composed peripheral bindings.
    #[must_use]
    pub fn uses_controller(&self, peripheral: &str) -> bool {
        self.dsi_hosts
            .values()
            .any(|binding| binding.peripheral == peripheral)
            || self
                .mipi_csi_hosts
                .values()
                .any(|binding| binding.peripheral == peripheral)
            || self
                .sdmmc_devices
                .values()
                .any(|binding| binding.peripheral == peripheral)
            || self
                .spi_buses
                .values()
                .any(|binding| binding.peripheral == peripheral)
            || self
                .spi_devices
                .values()
                .any(|binding| binding.peripheral == peripheral)
            || self
                .quad_spi_devices
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
            || self
                .capacitive_touch
                .values()
                .any(|binding| binding.peripheral == peripheral)
    }

    /// Returns whether a fixed built-in declaration consumes this DMA channel.
    ///
    /// Platform-owned runtime DMA pools use this to remove channels already
    /// moved into camera or I2S peripheral bindings.
    #[must_use]
    pub fn uses_dma(&self, dma: &str) -> bool {
        self.camera_captures
            .values()
            .any(|binding| binding.dma == dma)
            || self.i2s_streams.values().any(|binding| binding.dma == dma)
    }

    fn validate(&self) -> Result<(), ConfigError> {
        for (name, host) in &self.dsi_hosts {
            validate_resource_name("internal dsi-host", name)?;
            host.validate(name)?;
        }
        for (name, host) in &self.mipi_csi_hosts {
            validate_resource_name("internal mipi-csi", name)?;
            host.validate(name, &self.i2c_devices)?;
        }
        for (name, device) in &self.sdmmc_devices {
            validate_resource_name("internal sdmmc-device", name)?;
            device.validate(name)?;
        }
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
        for (name, spi) in &self.quad_spi_devices {
            validate_resource_name("internal quad-spi-device", name)?;
            spi.validate(name)?;
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
        for (name, touch) in &self.capacitive_touch {
            validate_resource_name("internal capacitive-touch", name)?;
            touch.validate(name)?;
        }
        Ok(())
    }
}

/// One fixed pair of SoC capacitive-touch channels.
#[derive(Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CapacitiveTouchDefinition {
    peripheral: String,
    #[serde(rename = "rtc-control")]
    rtc_control: String,
    #[serde(rename = "rtc-io")]
    rtc_io: String,
    pins: [String; 2],
}

impl CapacitiveTouchDefinition {
    /// Returns the chip-native touch controller singleton.
    #[must_use]
    pub fn peripheral(&self) -> &str {
        &self.peripheral
    }

    /// Returns the low-power controller containing the touch FSM.
    #[must_use]
    pub fn rtc_control(&self) -> &str {
        &self.rtc_control
    }

    /// Returns the RTC-I/O controller containing the touch pad muxes.
    #[must_use]
    pub fn rtc_io(&self) -> &str {
        &self.rtc_io
    }

    /// Returns the two touch-capable pins in stable button order.
    #[must_use]
    pub fn pins(&self) -> &[String; 2] {
        &self.pins
    }

    fn validate(&self, name: &str) -> Result<(), ConfigError> {
        validate_identifier(name, "peripheral", &self.peripheral)?;
        validate_identifier(name, "rtc-control", &self.rtc_control)?;
        validate_identifier(name, "rtc-io", &self.rtc_io)?;
        for pin in &self.pins {
            validate_identifier(name, "pin", pin)?;
        }
        Ok(())
    }
}

/// One MIPI CSI host sharing the Board's camera-control I2C bus.
#[derive(Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct MipiCsiDefinition {
    peripheral: String,
    i2c: String,
}

impl MipiCsiDefinition {
    /// Returns the chip-native CSI receiver singleton.
    #[must_use]
    pub fn peripheral(&self) -> &str {
        &self.peripheral
    }

    /// Returns the Board-local shared I2C bus used for SCCB discovery.
    #[must_use]
    pub fn i2c(&self) -> &str {
        &self.i2c
    }

    fn validate(
        &self,
        name: &str,
        i2c_devices: &BTreeMap<String, I2cDefinition>,
    ) -> Result<(), ConfigError> {
        validate_identifier(name, "peripheral", &self.peripheral)?;
        validate_resource_name("MIPI CSI I2C", &self.i2c)?;
        if !i2c_devices.contains_key(&self.i2c) {
            return Err(ConfigError::UnknownInternalI2c {
                resource: name.to_owned(),
                i2c: self.i2c.clone(),
            });
        }
        Ok(())
    }
}

/// One MIPI DSI host and its fixed Board power/backlight resources.
#[derive(Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct DsiHostDefinition {
    peripheral: String,
    dma: String,
    backlight: String,
    #[serde(rename = "data-lanes")]
    data_lanes: u8,
    #[serde(rename = "phy-power-channel")]
    phy_power_channel: u8,
    #[serde(rename = "phy-power-millivolts")]
    phy_power_millivolts: u16,
}

impl DsiHostDefinition {
    /// Returns the chip-native DSI controller singleton.
    #[must_use]
    pub fn peripheral(&self) -> &str {
        &self.peripheral
    }

    /// Returns the VDMA channel dedicated to display scanout.
    #[must_use]
    pub fn dma(&self) -> &str {
        &self.dma
    }

    /// Returns the panel backlight PWM pin.
    #[must_use]
    pub fn backlight(&self) -> &str {
        &self.backlight
    }

    /// Returns the physical MIPI DSI data-lane count.
    #[must_use]
    pub const fn data_lanes(&self) -> u8 {
        self.data_lanes
    }

    /// Returns the on-chip LDO channel supplying the DSI PHY.
    #[must_use]
    pub const fn phy_power_channel(&self) -> u8 {
        self.phy_power_channel
    }

    /// Returns the DSI PHY supply voltage in millivolts.
    #[must_use]
    pub const fn phy_power_millivolts(&self) -> u16 {
        self.phy_power_millivolts
    }

    fn validate(&self, name: &str) -> Result<(), ConfigError> {
        validate_identifier(name, "peripheral", &self.peripheral)?;
        validate_identifier(name, "dma", &self.dma)?;
        validate_identifier(name, "backlight", &self.backlight)?;
        if self.data_lanes == 0
            || self.data_lanes > 4
            || self.phy_power_channel == 0
            || self.phy_power_millivolts == 0
        {
            return Err(ConfigError::InvalidDsiHost {
                resource: name.to_owned(),
            });
        }
        Ok(())
    }
}

/// One fixed-width SDMMC card slot and optional on-chip power supply.
#[derive(Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SdmmcDeviceDefinition {
    peripheral: String,
    clk: String,
    cmd: String,
    data: Vec<String>,
    #[serde(rename = "bus-width")]
    bus_width: u8,
    #[serde(rename = "power-channel")]
    power_channel: Option<u8>,
    #[serde(rename = "power-millivolts")]
    power_millivolts: Option<u16>,
}

impl SdmmcDeviceDefinition {
    /// Returns the chip-native SDMMC controller singleton.
    #[must_use]
    pub fn peripheral(&self) -> &str {
        &self.peripheral
    }

    /// Returns the SD clock pin.
    #[must_use]
    pub fn clk(&self) -> &str {
        &self.clk
    }

    /// Returns the bidirectional SD command pin.
    #[must_use]
    pub fn cmd(&self) -> &str {
        &self.cmd
    }

    /// Returns D0 through D3 in protocol order.
    #[must_use]
    pub fn data(&self) -> &[String] {
        &self.data
    }

    /// Returns the configured SD bus width.
    #[must_use]
    pub const fn bus_width(&self) -> u8 {
        self.bus_width
    }

    /// Returns the on-chip LDO channel supplying the card slot.
    #[must_use]
    pub const fn power_channel(&self) -> Option<u8> {
        self.power_channel
    }

    /// Returns the card-slot supply voltage in millivolts.
    #[must_use]
    pub const fn power_millivolts(&self) -> Option<u16> {
        self.power_millivolts
    }

    fn validate(&self, name: &str) -> Result<(), ConfigError> {
        validate_identifier(name, "peripheral", &self.peripheral)?;
        validate_identifier(name, "clk", &self.clk)?;
        validate_identifier(name, "cmd", &self.cmd)?;
        for pin in &self.data {
            validate_identifier(name, "data", pin)?;
        }
        let power_valid = match (self.power_channel, self.power_millivolts) {
            (Some(channel), Some(millivolts)) => channel != 0 && millivolts != 0,
            (None, None) => true,
            _ => false,
        };
        if !matches!(self.bus_width, 1 | 4)
            || usize::from(self.bus_width) != self.data.len()
            || !power_valid
            || (self.bus_width == 4 && self.power_channel.is_none())
        {
            return Err(ConfigError::InvalidSdmmcDevice {
                resource: name.to_owned(),
            });
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

/// I/O declarations that become visible outside attached peripherals.
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

/// One statically wired I2C controller used by a peripheral implementation.
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

/// One statically wired SPI controller used by a peripheral implementation.
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

/// One internal SPI device, including the chip-select owned by its implementation.
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

/// One internal four-data-line half-duplex SPI device.
#[derive(Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct QuadSpiDeviceDefinition {
    peripheral: String,
    sck: String,
    data: [String; 4],
    #[serde(rename = "chip-select")]
    chip_select: String,
    #[serde(rename = "frequency-hz")]
    frequency_hz: u32,
}

impl QuadSpiDeviceDefinition {
    /// Returns the chip-native SPI controller identifier.
    #[must_use]
    pub fn peripheral(&self) -> &str {
        &self.peripheral
    }

    /// Returns the clock pin identifier.
    #[must_use]
    pub fn sck(&self) -> &str {
        &self.sck
    }

    /// Returns SIO0 through SIO3 in protocol order.
    #[must_use]
    pub fn data(&self) -> &[String; 4] {
        &self.data
    }

    /// Returns the chip-select pin identifier.
    #[must_use]
    pub fn chip_select(&self) -> &str {
        &self.chip_select
    }

    /// Returns the bus frequency.
    #[must_use]
    pub const fn frequency_hz(&self) -> u32 {
        self.frequency_hz
    }

    fn validate(&self, name: &str) -> Result<(), ConfigError> {
        validate_identifier(name, "peripheral", &self.peripheral)?;
        validate_identifier(name, "sck", &self.sck)?;
        validate_identifier(name, "chip-select", &self.chip_select)?;
        for pin in &self.data {
            validate_identifier(name, "data", pin)?;
        }
        validate_frequency(name, self.frequency_hz)
    }
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

/// One optional Board peripheral and the implementation used to construct it.
#[derive(Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PeripheralDefinition {
    implementation: String,
    #[serde(default)]
    bindings: BTreeMap<String, String>,
    #[serde(default)]
    parameters: BTreeMap<String, PeripheralParameter>,
}

impl PeripheralDefinition {
    /// Returns the stable peripheral implementation identifier.
    #[must_use]
    pub fn implementation(&self) -> &str {
        &self.implementation
    }

    /// Finds one chip-native binding by its implementation-defined role.
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

    /// Finds one implementation-defined construction parameter.
    #[must_use]
    pub fn parameter(&self, name: &str) -> Option<&PeripheralParameter> {
        self.parameters.get(name)
    }

    /// Iterates implementation parameters in stable name order.
    pub fn parameters(&self) -> impl Iterator<Item = (&str, &PeripheralParameter)> {
        self.parameters
            .iter()
            .map(|(name, value)| (name.as_str(), value))
    }
}

/// Build-time value passed to a peripheral implementation constructor.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(untagged)]
pub enum PeripheralParameter {
    /// Boolean setting.
    Boolean(bool),
    /// Signed integer setting.
    Integer(i64),
    /// Text setting or chip-native identifier interpreted by the implementation.
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
    #[serde(default, rename = "flash-size")]
    flash_size: Option<String>,
    #[serde(default, rename = "external-memory")]
    external_memory: Option<ExternalMemoryDefinition>,
}

impl HardwareDefinition {
    /// Returns the canonical HAL chip name.
    #[must_use]
    pub fn chip(&self) -> &str {
        &self.chip
    }

    /// Returns the physical flash capacity in the syntax expected by the
    /// Platform flasher, when the Board declares one.
    #[must_use]
    pub fn flash_size(&self) -> Option<&str> {
        self.flash_size.as_deref()
    }

    /// Returns directly addressable external memory installed on the Board.
    #[must_use]
    pub const fn external_memory(&self) -> Option<&ExternalMemoryDefinition> {
        self.external_memory.as_ref()
    }
}

/// Directly addressable external memory installed on a Board.
#[derive(Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ExternalMemoryDefinition {
    technology: ExternalMemoryTechnologyDefinition,
    interface: ExternalMemoryInterfaceDefinition,
    #[serde(rename = "size-bytes")]
    size_bytes: usize,
}

impl ExternalMemoryDefinition {
    /// Returns the physical memory technology.
    #[must_use]
    pub const fn technology(&self) -> ExternalMemoryTechnologyDefinition {
        self.technology
    }

    /// Returns the physical interface used by the chip.
    #[must_use]
    pub const fn interface(&self) -> ExternalMemoryInterfaceDefinition {
        self.interface
    }

    /// Returns the installed capacity in bytes.
    #[must_use]
    pub const fn size_bytes(&self) -> usize {
        self.size_bytes
    }
}

/// Parsed external-memory technology.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum ExternalMemoryTechnologyDefinition {
    /// Pseudo-static RAM.
    Psram,
}

/// Parsed external-memory electrical interface.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum ExternalMemoryInterfaceDefinition {
    /// Four-data-line SPI.
    QuadSpi,
    /// Eight-data-line SPI.
    OctalSpi,
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
    /// A Board declared an external-memory device with no capacity.
    #[error("Board external-memory size must be greater than zero bytes")]
    ZeroExternalMemory,
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
    /// A DSI host declared no lanes, no PHY supply, or an unsupported lane count.
    #[error("Board DSI host `{resource}` has an invalid lane or PHY-power configuration")]
    InvalidDsiHost {
        /// Board-level DSI host resource name.
        resource: String,
    },
    /// An SDMMC slot has an unsupported width or no power supply.
    #[error("Board SDMMC device `{resource}` has an invalid bus or power configuration")]
    InvalidSdmmcDevice {
        /// Board-level SDMMC resource name.
        resource: String,
    },
    /// A MIPI CSI host references an undeclared shared I2C bus.
    #[error("Board MIPI CSI host `{resource}` references unknown I2C bus `{i2c}`")]
    UnknownInternalI2c {
        /// Board-level MIPI CSI resource name.
        resource: String,
        /// Missing Board-local I2C resource name.
        i2c: String,
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
    let hardware = if let Some(memory) = board.hardware().external_memory() {
        let technology = match memory.technology() {
            ExternalMemoryTechnologyDefinition::Psram => {
                "::barracuda_board::ExternalMemoryTechnology::Psram"
            }
        };
        let interface = match memory.interface() {
            ExternalMemoryInterfaceDefinition::QuadSpi => {
                "::barracuda_board::ExternalMemoryInterface::QuadSpi"
            }
            ExternalMemoryInterfaceDefinition::OctalSpi => {
                "::barracuda_board::ExternalMemoryInterface::OctalSpi"
            }
        };
        format!(
            "::barracuda_board::Hardware::new({:?}).with_external_memory(\n        ::barracuda_board::ExternalMemory::new({technology}, {interface}, {}),\n    )",
            board.hardware().chip(),
            memory.size_bytes(),
        )
    } else {
        format!(
            "::barracuda_board::Hardware::new({:?})",
            board.hardware().chip()
        )
    };
    format!(
        "/// Board selected by the build configuration.\n\
         pub const BOARD: ::barracuda_board::Board = ::barracuda_board::Board::new(\n    {:?},\n    {hardware},\n    ::barracuda_board::NativeLayout::new({:?}),\n);\n",
        board.name(),
        board.native_layout().artifact(),
    )
}
