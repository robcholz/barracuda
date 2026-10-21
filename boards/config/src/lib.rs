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
    #[serde(default)]
    profile: Option<String>,
    #[serde(default)]
    plugins: BTreeMap<String, PluginSection>,
}

/// One Board's `plugins.<id>` section: whether the Board enables the Plugin,
/// how its peripheral slots are wired, and its settings.
#[derive(Debug, Default, Deserialize, PartialEq, Eq)]
struct PluginSection {
    #[serde(default)]
    enabled: Option<bool>,
    #[serde(default)]
    peripherals: BTreeMap<String, String>,
    #[serde(flatten)]
    settings: BTreeMap<String, PluginSetting>,
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

    /// Returns the settings this Board gives one Plugin, in name order.
    ///
    /// The Board only stores them: each Plugin declares which settings it
    /// accepts, and its build checks them.
    pub fn plugin_settings(&self, plugin: &str) -> impl Iterator<Item = (&str, &PluginSetting)> {
        self.plugins
            .get(plugin)
            .into_iter()
            .flat_map(|section| section.settings.iter())
            .map(|(name, setting)| (name.as_str(), setting))
    }

    /// Returns the Plugin profile this Board starts from, if it names one.
    #[must_use]
    pub fn profile(&self) -> Option<&str> {
        self.profile.as_deref()
    }

    /// Returns whether the Board explicitly enables (`Some(true)`) or disables
    /// (`Some(false)`) one Plugin, overriding its profile.
    #[must_use]
    pub fn plugin_enabled(&self, plugin: &str) -> Option<bool> {
        self.plugins.get(plugin).and_then(|section| section.enabled)
    }

    /// Returns the peripheral wiring this Board gives one Plugin: slot
    /// (`<kind>.<slot>`) to device name, in slot order.
    pub fn plugin_peripherals(&self, plugin: &str) -> impl Iterator<Item = (&str, &str)> {
        self.plugins
            .get(plugin)
            .into_iter()
            .flat_map(|section| section.peripherals.iter())
            .map(|(slot, device)| (slot.as_str(), device.as_str()))
    }

    /// Iterates the identities of Plugins this Board configures.
    pub fn configured_plugins(&self) -> impl Iterator<Item = &str> {
        self.plugins.keys().map(String::as_str)
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
        if let Some(ethernet) = &self.hardware.ethernet {
            ethernet.validate()?;
        }
        if let Some(coprocessor) = &self.hardware.wifi_coprocessor {
            coprocessor.validate()?;
            // The co-processor's SDIO link holds its slot of the chip's SD
            // host for the life of the firmware; a card cannot share it.
            let slot = coprocessor.sdio.slot;
            if let Some((name, _)) = self
                .peripherals
                .io
                .sdmmc_devices
                .iter()
                .find(|(_, device)| device.slot == slot)
            {
                return Err(ConfigError::SdmmcSlotInUse {
                    resource: name.clone(),
                    slot,
                    user: String::from("the Wi-Fi co-processor"),
                });
            }
        }
        if let Some(pin) = &self.hardware.power_hold {
            validate_identifier("power-hold", "pin", pin)?;
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
        if let Some(profile) = &self.profile {
            validate_resource_name("profile", profile)?;
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
    #[serde(default, rename = "parallel-output")]
    parallel_outputs: BTreeMap<String, ParallelOutputDefinition>,
    #[serde(default, rename = "i2s-stream")]
    i2s_streams: BTreeMap<String, I2sStreamDefinition>,
    #[serde(default, rename = "capacitive-touch")]
    capacitive_touch: BTreeMap<String, CapacitiveTouchDefinition>,
    #[serde(default, rename = "analog-input")]
    analog_inputs: BTreeMap<String, AnalogInputDefinition>,
    #[serde(default, rename = "pdm-i2s-stream")]
    pdm_i2s_streams: BTreeMap<String, PdmI2sStreamDefinition>,
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
            && self.parallel_outputs.is_empty()
            && self.i2s_streams.is_empty()
            && self.capacitive_touch.is_empty()
            && self.analog_inputs.is_empty()
            && self.pdm_i2s_streams.is_empty()
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

    /// Iterates every declared SDMMC device in stable Board-name order.
    pub fn sdmmc_devices(&self) -> impl Iterator<Item = (&str, &SdmmcDeviceDefinition)> {
        self.sdmmc_devices
            .iter()
            .map(|(name, device)| (name.as_str(), device))
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

    /// Finds one eight-bit parallel output reserved by a peripheral
    /// implementation.
    #[must_use]
    pub fn parallel_output(&self, name: &str) -> Option<&ParallelOutputDefinition> {
        self.parallel_outputs.get(name)
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

    /// Finds one analog input channel reserved by a peripheral implementation.
    #[must_use]
    pub fn analog_input(&self, name: &str) -> Option<&AnalogInputDefinition> {
        self.analog_inputs.get(name)
    }

    /// Iterates every declared analog input in stable Board-name order.
    pub fn analog_inputs(&self) -> impl Iterator<Item = (&str, &AnalogInputDefinition)> {
        self.analog_inputs
            .iter()
            .map(|(name, input)| (name.as_str(), input))
    }

    /// Finds one PDM microphone and I2S amplifier stream reserved by a
    /// peripheral implementation.
    #[must_use]
    pub fn pdm_i2s_stream(&self, name: &str) -> Option<&PdmI2sStreamDefinition> {
        self.pdm_i2s_streams.get(name)
    }

    /// Returns how many `spi-device` declarations share the SPI controller
    /// `peripheral`. Devices on one controller share its clock and data
    /// lines, each with its own chip select and clock rate.
    #[must_use]
    pub fn spi_devices_on(&self, peripheral: &str) -> usize {
        self.spi_devices
            .values()
            .filter(|device| device.peripheral == peripheral)
            .count()
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
                .parallel_outputs
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
            || self
                .analog_inputs
                .values()
                .any(|binding| binding.peripheral == peripheral)
            || self
                .pdm_i2s_streams
                .values()
                .any(|binding| binding.peripheral == peripheral)
    }

    /// Returns whether a fixed built-in declaration consumes this DMA channel.
    ///
    /// Platform-owned runtime DMA pools use this to remove channels already
    /// moved into camera, parallel-output, or I2S peripheral bindings.
    #[must_use]
    pub fn uses_dma(&self, dma: &str) -> bool {
        self.camera_captures
            .values()
            .any(|binding| binding.dma == dma)
            || self
                .parallel_outputs
                .values()
                .any(|binding| binding.dma == dma)
            || self.i2s_streams.values().any(|binding| binding.dma == dma)
            || self
                .pdm_i2s_streams
                .values()
                .any(|binding| binding.dma == dma)
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
            // One slot of an SD host carries one card.
            if let Some((other, _)) = self.sdmmc_devices.iter().find(|(other, candidate)| {
                *other < name
                    && candidate.peripheral == device.peripheral
                    && candidate.slot == device.slot
            }) {
                return Err(ConfigError::SdmmcSlotInUse {
                    resource: name.clone(),
                    slot: device.slot,
                    user: format!("sdmmc-device `{other}`"),
                });
            }
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
            if spi.three_wire && (spi.mosi.is_none() || spi.miso.is_some()) {
                return Err(ConfigError::InvalidThreeWireSpi {
                    resource: name.clone(),
                });
            }
            if let Some(read_frequency_hz) = spi.read_frequency_hz {
                if !spi.three_wire {
                    return Err(ConfigError::InvalidThreeWireSpi {
                        resource: name.clone(),
                    });
                }
                validate_frequency(name, read_frequency_hz)?;
            }
            validate_frequency(name, spi.frequency_hz)?;
            // Devices on one controller share its clock and data lines; each
            // keeps its own chip select and clock rate.
            if let Some((other, _)) = self.spi_devices.iter().find(|(other, candidate)| {
                *other != name
                    && candidate.peripheral == spi.peripheral
                    && (candidate.sck != spi.sck
                        || candidate.mosi != spi.mosi
                        || candidate.miso != spi.miso
                        || candidate.three_wire
                        || spi.three_wire)
            }) {
                return Err(ConfigError::InconsistentSharedSpi {
                    resource: name.clone(),
                    other: other.clone(),
                });
            }
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
        for (name, output) in &self.parallel_outputs {
            validate_resource_name("internal parallel-output", name)?;
            output.validate(name)?;
        }
        for (name, stream) in &self.i2s_streams {
            validate_resource_name("internal i2s-stream", name)?;
            stream.validate(name)?;
        }
        for (name, touch) in &self.capacitive_touch {
            validate_resource_name("internal capacitive-touch", name)?;
            touch.validate(name)?;
        }
        for (name, input) in &self.analog_inputs {
            validate_resource_name("internal analog-input", name)?;
            validate_identifier(name, "peripheral", &input.peripheral)?;
            validate_identifier(name, "pin", &input.pin)?;
        }
        for (name, stream) in &self.pdm_i2s_streams {
            validate_resource_name("internal pdm-i2s-stream", name)?;
            stream.validate(name)?;
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
///
/// `backlight` is the panel backlight pin when the chip drives it directly. A
/// panel whose backlight sits behind another chip, such as an I/O expander,
/// omits it, and its display implementation drives that chip.
#[derive(Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct DsiHostDefinition {
    peripheral: String,
    dma: String,
    #[serde(default)]
    backlight: Option<String>,
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

    /// Returns the panel backlight pin, when the chip drives it directly.
    #[must_use]
    pub fn backlight(&self) -> Option<&str> {
        self.backlight.as_deref()
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
        if let Some(backlight) = &self.backlight {
            validate_identifier(name, "backlight", backlight)?;
        }
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
///
/// `slot` names the slot of the chip's SD host the card is wired to; it
/// defaults to 0. The selected Platform lists the slots it hands to a Board
/// and the supply each needs (`hal.sdmmc-slots` in `platform.yml`).
#[derive(Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SdmmcDeviceDefinition {
    peripheral: String,
    #[serde(default)]
    slot: u8,
    clk: String,
    cmd: String,
    data: Vec<String>,
    #[serde(rename = "bus-width")]
    bus_width: u8,
    #[serde(rename = "power-channel")]
    power_channel: Option<u8>,
    #[serde(rename = "power-millivolts")]
    power_millivolts: Option<u16>,
    #[serde(default, rename = "frequency-hz")]
    frequency_hz: Option<u32>,
}

/// The SD card clock a Board that names none runs its card at: the top of
/// the default bus speed mode.
pub const DEFAULT_SDMMC_FREQUENCY_HZ: u32 = 25_000_000;

impl SdmmcDeviceDefinition {
    /// Returns the chip-native SDMMC controller singleton.
    #[must_use]
    pub fn peripheral(&self) -> &str {
        &self.peripheral
    }

    /// Returns the slot of the SD host the card is wired to.
    #[must_use]
    pub const fn slot(&self) -> u8 {
        self.slot
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

    /// Returns the card clock once the card is identified: the Board's
    /// `frequency-hz`, or [`DEFAULT_SDMMC_FREQUENCY_HZ`]. Above 25 MHz the
    /// card is switched to high speed, and stays at 25 MHz if it refuses.
    #[must_use]
    pub fn frequency_hz(&self) -> u32 {
        self.frequency_hz.unwrap_or(DEFAULT_SDMMC_FREQUENCY_HZ)
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
        // SD high speed tops out at 50 MHz.
        if self.slot > 1
            || !(1..=50_000_000).contains(&self.frequency_hz())
            || !matches!(self.bus_width, 1 | 4)
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

/// One eight-bit Intel 8080-style parallel output and its fixed Board wiring:
/// a write strobe, an active-low chip select asserted for each burst, eight
/// data lines, and an optional data/command line.
///
/// A MIPI DBI panel controller wires all of them and tells commands from
/// data by `dc`. A directly driven e-paper source driver takes only data: its
/// source clock is `wr`, its source start pulse is `chip-select`, and `dc` is
/// left out.
#[derive(Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ParallelOutputDefinition {
    peripheral: String,
    dma: String,
    wr: String,
    #[serde(default)]
    dc: Option<String>,
    #[serde(rename = "chip-select")]
    chip_select: String,
    data: [String; 8],
    #[serde(rename = "frequency-hz")]
    frequency_hz: u32,
    #[serde(rename = "dma-buffer-bytes")]
    dma_buffer_bytes: usize,
}

impl ParallelOutputDefinition {
    /// Returns the chip-native parallel-output controller.
    #[must_use]
    pub fn peripheral(&self) -> &str {
        &self.peripheral
    }
    /// Returns the chip-native DMA channel.
    #[must_use]
    pub fn dma(&self) -> &str {
        &self.dma
    }
    /// Returns the write-strobe pin.
    #[must_use]
    pub fn wr(&self) -> &str {
        &self.wr
    }
    /// Returns the data/command pin, when the Board wires one.
    #[must_use]
    pub fn dc(&self) -> Option<&str> {
        self.dc.as_deref()
    }
    /// Returns the active-low chip-select pin, asserted for each burst.
    #[must_use]
    pub fn chip_select(&self) -> &str {
        &self.chip_select
    }
    /// Returns the eight data pins in least-significant-bit order.
    #[must_use]
    pub fn data(&self) -> &[String; 8] {
        &self.data
    }
    /// Returns the write-strobe frequency.
    #[must_use]
    pub const fn frequency_hz(&self) -> u32 {
        self.frequency_hz
    }
    /// Returns the statically allocated DMA buffer capacity, the longest
    /// burst clocked out under one chip-select assertion.
    #[must_use]
    pub const fn dma_buffer_bytes(&self) -> usize {
        self.dma_buffer_bytes
    }

    fn validate(&self, name: &str) -> Result<(), ConfigError> {
        for (field, value) in [
            ("peripheral", self.peripheral.as_str()),
            ("dma", self.dma.as_str()),
            ("wr", self.wr.as_str()),
            ("chip-select", self.chip_select.as_str()),
        ] {
            validate_identifier(name, field, value)?;
        }
        if let Some(dc) = &self.dc {
            validate_identifier(name, "dc", dc)?;
        }
        for pin in &self.data {
            validate_identifier(name, "data", pin)?;
        }
        validate_frequency(name, self.frequency_hz)?;
        if self.dma_buffer_bytes == 0 {
            return Err(ConfigError::ZeroDmaBuffer {
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

/// One analog input pin sampled by a SoC ADC controller.
///
/// Several inputs may name one controller; the Board composition builds the
/// converter once and hands each implementation its own channel.
#[derive(Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AnalogInputDefinition {
    peripheral: String,
    pin: String,
}

impl AnalogInputDefinition {
    /// Returns the chip-native ADC controller.
    #[must_use]
    pub fn peripheral(&self) -> &str {
        &self.peripheral
    }

    /// Returns the analog-capable pin the controller samples.
    #[must_use]
    pub fn pin(&self) -> &str {
        &self.pin
    }
}

/// One I2S controller that records from a PDM microphone and plays to an I2S
/// amplifier, one direction at a time.
///
/// M5Stack products wire the microphone's clock and the amplifier's word
/// select to one pin, so the controller switches between PDM receive and
/// standard I2S transmit for each transfer. Either side may be absent: a
/// microphone-only product leaves out `bclk` and `dout`, an amplifier-only
/// one leaves out `din`.
#[derive(Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PdmI2sStreamDefinition {
    peripheral: String,
    dma: String,
    clk: String,
    #[serde(default)]
    din: Option<String>,
    #[serde(default)]
    bclk: Option<String>,
    #[serde(default)]
    dout: Option<String>,
    #[serde(rename = "sample-rate-hz")]
    sample_rate_hz: u32,
    #[serde(rename = "dma-buffer-bytes")]
    dma_buffer_bytes: usize,
}

impl PdmI2sStreamDefinition {
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
    /// Returns the microphone clock pin, also the amplifier's word select.
    #[must_use]
    pub fn clk(&self) -> &str {
        &self.clk
    }
    /// Returns the microphone data pin, when a microphone is fitted.
    #[must_use]
    pub fn din(&self) -> Option<&str> {
        self.din.as_deref()
    }
    /// Returns the amplifier bit-clock pin, when an amplifier is fitted.
    #[must_use]
    pub fn bclk(&self) -> Option<&str> {
        self.bclk.as_deref()
    }
    /// Returns the amplifier data pin, when an amplifier is fitted.
    #[must_use]
    pub fn dout(&self) -> Option<&str> {
        self.dout.as_deref()
    }
    /// Returns the PCM sample rate of both directions.
    #[must_use]
    pub const fn sample_rate_hz(&self) -> u32 {
        self.sample_rate_hz
    }
    /// Returns each statically allocated DMA buffer's capacity.
    #[must_use]
    pub const fn dma_buffer_bytes(&self) -> usize {
        self.dma_buffer_bytes
    }

    fn validate(&self, name: &str) -> Result<(), ConfigError> {
        for (field, value) in [
            ("peripheral", Some(self.peripheral.as_str())),
            ("dma", Some(self.dma.as_str())),
            ("clk", Some(self.clk.as_str())),
            ("din", self.din.as_deref()),
            ("bclk", self.bclk.as_deref()),
            ("dout", self.dout.as_deref()),
        ] {
            if let Some(value) = value {
                validate_identifier(name, field, value)?;
            }
        }
        // The amplifier needs both of its lines, and the stream at least one
        // direction.
        if self.bclk.is_some() != self.dout.is_some() || (self.din.is_none() && self.dout.is_none())
        {
            return Err(ConfigError::InvalidPdmI2sStream {
                resource: name.to_owned(),
            });
        }
        validate_frequency(name, self.sample_rate_hz)?;
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
    #[serde(default, rename = "three-wire")]
    three_wire: bool,
    #[serde(default, rename = "read-frequency-hz")]
    read_frequency_hz: Option<u32>,
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

    /// Returns whether `mosi` is one bidirectional data line (three-wire SPI)
    /// on which the device also answers reads.
    #[must_use]
    pub const fn three_wire(&self) -> bool {
        self.three_wire
    }

    /// Returns the clock used for reads on a three-wire device, which
    /// defaults to the write clock.
    #[must_use]
    pub fn read_frequency_hz(&self) -> u32 {
        self.read_frequency_hz.unwrap_or(self.frequency_hz)
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
    primary: bool,
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

    /// Returns whether the Board marks this device as the primary one of its
    /// kind. A kind with one device has that device as primary without a mark.
    #[must_use]
    pub const fn primary(&self) -> bool {
        self.primary
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

/// One Plugin setting given by a Board.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(untagged)]
pub enum PluginSetting {
    /// Boolean setting.
    Boolean(bool),
    /// Integer setting.
    Integer(i64),
    /// Text setting.
    String(String),
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
    #[serde(default)]
    ethernet: Option<EthernetDefinition>,
    #[serde(default, rename = "wifi-coprocessor")]
    wifi_coprocessor: Option<WifiCoprocessorDefinition>,
    #[serde(default, rename = "power-hold")]
    power_hold: Option<String>,
}

impl HardwareDefinition {
    /// Returns the chip pin that latches the Board's battery power on, when
    /// the Board has one: the Platform entry drives it high before anything
    /// else, or the Board switches itself off when its power button is
    /// released.
    #[must_use]
    pub fn power_hold(&self) -> Option<&str> {
        self.power_hold.as_deref()
    }

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

    /// Returns the wired Ethernet PHY attached to the chip's MAC, when the
    /// Board has one.
    #[must_use]
    pub const fn ethernet(&self) -> Option<&EthernetDefinition> {
        self.ethernet.as_ref()
    }

    /// Returns the Wi-Fi co-processor wired to the chip, when the Board has
    /// one.
    #[must_use]
    pub const fn wifi_coprocessor(&self) -> Option<&WifiCoprocessorDefinition> {
        self.wifi_coprocessor.as_ref()
    }
}

/// Wired Ethernet: the chip's Ethernet MAC connected to an external PHY.
///
/// Every field is a hardware fact read from the Board schematic. The selected
/// Platform owns the MAC, builds its IP link on it, and rejects a Board whose
/// interface it cannot drive.
#[derive(Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct EthernetDefinition {
    phy: EthernetPhyDefinition,
    rmii: RmiiDefinition,
}

impl EthernetDefinition {
    /// Returns the PHY chip and its management address.
    #[must_use]
    pub const fn phy(&self) -> &EthernetPhyDefinition {
        &self.phy
    }

    /// Returns the RMII wiring between the MAC and the PHY.
    #[must_use]
    pub const fn rmii(&self) -> &RmiiDefinition {
        &self.rmii
    }

    /// Iterates every chip pin the Ethernet wiring uses, with its signal name.
    pub fn pins(&self) -> impl Iterator<Item = (&'static str, &str)> {
        let rmii = &self.rmii;
        [
            (
                "reference-clock input",
                Some(rmii.reference_clock.input.as_str()),
            ),
            (
                "reference-clock output",
                rmii.reference_clock.output.as_deref(),
            ),
            ("mdc", Some(rmii.mdc.as_str())),
            ("mdio", Some(rmii.mdio.as_str())),
            ("tx-en", Some(rmii.tx_en.as_str())),
            ("txd0", Some(rmii.txd0.as_str())),
            ("txd1", Some(rmii.txd1.as_str())),
            ("crs-dv", Some(rmii.crs_dv.as_str())),
            ("rxd0", Some(rmii.rxd0.as_str())),
            ("rxd1", Some(rmii.rxd1.as_str())),
            ("phy reset", self.phy.reset.as_deref()),
        ]
        .into_iter()
        .filter_map(|(field, pin)| pin.map(|pin| (field, pin)))
    }

    fn validate(&self) -> Result<(), ConfigError> {
        let resource = "ethernet";
        validate_identifier(resource, "phy model", &self.phy.model)?;
        if self.phy.address > 31 {
            return Err(ConfigError::InvalidEthernet {
                reason: "the PHY address must be a Clause 22 address from 0 to 31",
            });
        }
        let clock = &self.rmii.reference_clock;
        match (clock.source, clock.output.is_some()) {
            (RmiiReferenceClockSource::External, false)
            | (RmiiReferenceClockSource::Internal, true) => {}
            (RmiiReferenceClockSource::External, true) => {
                return Err(ConfigError::InvalidEthernet {
                    reason: "an external reference clock has no chip output pin",
                });
            }
            (RmiiReferenceClockSource::Internal, false) => {
                return Err(ConfigError::InvalidEthernet {
                    reason: "an internal reference clock needs the chip pin that drives it out",
                });
            }
        }
        let mut pins = Vec::new();
        for (field, pin) in self.pins() {
            validate_identifier(resource, field, pin)?;
            if pins.contains(&pin) {
                return Err(ConfigError::InvalidEthernet {
                    reason: "one chip pin is wired to two Ethernet signals",
                });
            }
            pins.push(pin);
        }
        Ok(())
    }
}

/// The Ethernet PHY chip on a Board.
#[derive(Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct EthernetPhyDefinition {
    model: String,
    address: u8,
    #[serde(default)]
    reset: Option<String>,
}

impl EthernetPhyDefinition {
    /// Returns the PHY part, such as `ip101gri`. It documents the hardware; a
    /// Platform that drives every PHY through IEEE 802.3 Clause 22 registers
    /// does not need it.
    #[must_use]
    pub fn model(&self) -> &str {
        &self.model
    }

    /// Returns the PHY's Clause 22 management address, set by its strap pins.
    #[must_use]
    pub const fn address(&self) -> u8 {
        self.address
    }

    /// Returns the chip pin wired to the PHY's active-low reset, when one is.
    #[must_use]
    pub fn reset(&self) -> Option<&str> {
        self.reset.as_deref()
    }
}

/// RMII wiring between the chip's MAC and the PHY, by chip-native pin names.
#[derive(Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RmiiDefinition {
    #[serde(rename = "reference-clock")]
    reference_clock: RmiiReferenceClockDefinition,
    mdc: String,
    mdio: String,
    #[serde(rename = "tx-en")]
    tx_en: String,
    txd0: String,
    txd1: String,
    #[serde(rename = "crs-dv")]
    crs_dv: String,
    rxd0: String,
    rxd1: String,
}

impl RmiiDefinition {
    /// Returns where the 50 MHz reference clock comes from.
    #[must_use]
    pub const fn reference_clock(&self) -> &RmiiReferenceClockDefinition {
        &self.reference_clock
    }

    /// Returns the management clock pin.
    #[must_use]
    pub fn mdc(&self) -> &str {
        &self.mdc
    }

    /// Returns the management data pin.
    #[must_use]
    pub fn mdio(&self) -> &str {
        &self.mdio
    }

    /// Returns the transmit-enable pin.
    #[must_use]
    pub fn tx_en(&self) -> &str {
        &self.tx_en
    }

    /// Returns the first transmit data pin.
    #[must_use]
    pub fn txd0(&self) -> &str {
        &self.txd0
    }

    /// Returns the second transmit data pin.
    #[must_use]
    pub fn txd1(&self) -> &str {
        &self.txd1
    }

    /// Returns the carrier-sense/receive-data-valid pin.
    #[must_use]
    pub fn crs_dv(&self) -> &str {
        &self.crs_dv
    }

    /// Returns the first receive data pin.
    #[must_use]
    pub fn rxd0(&self) -> &str {
        &self.rxd0
    }

    /// Returns the second receive data pin.
    #[must_use]
    pub fn rxd1(&self) -> &str {
        &self.rxd1
    }
}

/// The RMII 50 MHz reference clock and the chip pins it uses.
#[derive(Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RmiiReferenceClockDefinition {
    source: RmiiReferenceClockSource,
    input: String,
    #[serde(default)]
    output: Option<String>,
}

impl RmiiReferenceClockDefinition {
    /// Returns which side generates the clock.
    #[must_use]
    pub const fn source(&self) -> RmiiReferenceClockSource {
        self.source
    }

    /// Returns the chip pin the clock enters the MAC on.
    #[must_use]
    pub fn input(&self) -> &str {
        &self.input
    }

    /// Returns the chip pin that drives an internal clock out, looped back on
    /// the Board into `input`; `None` for an external clock.
    #[must_use]
    pub fn output(&self) -> Option<&str> {
        self.output.as_deref()
    }
}

/// Who generates the RMII reference clock.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum RmiiReferenceClockSource {
    /// The PHY or an oscillator on the Board drives it into the chip.
    External,
    /// The chip generates it and drives it out; the Board loops it back in.
    Internal,
}

/// A Wi-Fi radio on a separate co-processor chip, wired to this chip over
/// SDIO.
///
/// Every field is a hardware fact read from the Board schematic and the
/// firmware the co-processor ships with. The selected Platform drives the
/// co-processor as a network link and rejects a Board whose wiring or
/// firmware it cannot drive.
#[derive(Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WifiCoprocessorDefinition {
    chip: String,
    firmware: WifiCoprocessorFirmware,
    sdio: CoprocessorSdioDefinition,
    reset: String,
    #[serde(default, rename = "power-enable")]
    power_enable: Option<CoprocessorPowerEnableDefinition>,
}

impl WifiCoprocessorDefinition {
    /// Returns the co-processor chip, such as `esp32c6`.
    #[must_use]
    pub fn chip(&self) -> &str {
        &self.chip
    }

    /// Returns the protocol the co-processor's firmware speaks.
    #[must_use]
    pub const fn firmware(&self) -> WifiCoprocessorFirmware {
        self.firmware
    }

    /// Returns the SDIO wiring between this chip and the co-processor.
    #[must_use]
    pub const fn sdio(&self) -> &CoprocessorSdioDefinition {
        &self.sdio
    }

    /// Returns the chip pin wired to the co-processor's active-low enable.
    #[must_use]
    pub fn reset(&self) -> &str {
        &self.reset
    }

    /// Returns the switch that powers the co-processor, when the Board has
    /// one.
    #[must_use]
    pub const fn power_enable(&self) -> Option<&CoprocessorPowerEnableDefinition> {
        self.power_enable.as_ref()
    }

    /// Iterates every chip pin the co-processor wiring uses, with its signal
    /// name.
    pub fn pins(&self) -> impl Iterator<Item = (&'static str, &str)> {
        let sdio = &self.sdio;
        let power = self.power_enable.as_ref();
        [
            ("sdio clk", Some(sdio.clk.as_str())),
            ("sdio cmd", Some(sdio.cmd.as_str())),
            ("sdio d0", Some(sdio.d0.as_str())),
            ("sdio d1", Some(sdio.d1.as_str())),
            ("sdio d2", Some(sdio.d2.as_str())),
            ("sdio d3", Some(sdio.d3.as_str())),
            ("reset", Some(self.reset.as_str())),
            (
                "power-enable scl",
                power.map(|power| power.i2c.scl.as_str()),
            ),
            (
                "power-enable sda",
                power.map(|power| power.i2c.sda.as_str()),
            ),
        ]
        .into_iter()
        .filter_map(|(field, pin)| pin.map(|pin| (field, pin)))
    }

    fn validate(&self) -> Result<(), ConfigError> {
        let resource = "wifi-coprocessor";
        validate_identifier(resource, "chip", &self.chip)?;
        if self.sdio.slot > 1 {
            return Err(ConfigError::InvalidWifiCoprocessor {
                reason: "the SDIO slot must be 0 or 1",
            });
        }
        // SDIO high-speed mode tops out at 50 MHz.
        if self.sdio.frequency_hz == 0 || self.sdio.frequency_hz > 50_000_000 {
            return Err(ConfigError::InvalidWifiCoprocessor {
                reason: "the SDIO clock must be between 1 Hz and 50 MHz",
            });
        }
        if let Some(power) = &self.power_enable {
            validate_identifier(
                resource,
                "power-enable i2c peripheral",
                &power.i2c.peripheral,
            )?;
            if power.address > 0x7f {
                return Err(ConfigError::InvalidWifiCoprocessor {
                    reason: "the power-enable expander address must be a 7-bit I2C address",
                });
            }
            if power.pin > 7 {
                return Err(ConfigError::InvalidWifiCoprocessor {
                    reason: "the power-enable expander pin must be 0 through 7",
                });
            }
        }
        let mut pins = Vec::new();
        for (field, pin) in self.pins() {
            validate_identifier(resource, field, pin)?;
            if pins.contains(&pin) {
                return Err(ConfigError::InvalidWifiCoprocessor {
                    reason: "one chip pin is wired to two co-processor signals",
                });
            }
            pins.push(pin);
        }
        Ok(())
    }
}

/// The protocol a Wi-Fi co-processor's firmware speaks.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum WifiCoprocessorFirmware {
    /// Espressif's ESP-Hosted-MCU slave firmware.
    EspHostedMcu,
}

/// The 4-bit SDIO bus between the chip's SD host and the co-processor.
#[derive(Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CoprocessorSdioDefinition {
    slot: u8,
    clk: String,
    cmd: String,
    d0: String,
    d1: String,
    d2: String,
    d3: String,
    #[serde(rename = "frequency-hz")]
    frequency_hz: u32,
}

impl CoprocessorSdioDefinition {
    /// Returns the chip's SD host slot the bus is wired to.
    #[must_use]
    pub const fn slot(&self) -> u8 {
        self.slot
    }

    /// Returns the clock pin.
    #[must_use]
    pub fn clk(&self) -> &str {
        &self.clk
    }

    /// Returns the command pin.
    #[must_use]
    pub fn cmd(&self) -> &str {
        &self.cmd
    }

    /// Returns the four data pins, D0 first.
    #[must_use]
    pub fn data(&self) -> [&str; 4] {
        [&self.d0, &self.d1, &self.d2, &self.d3]
    }

    /// Returns the bus clock once the card is set up.
    #[must_use]
    pub const fn frequency_hz(&self) -> u32 {
        self.frequency_hz
    }
}

/// An I/O-expander output that switches the co-processor's supply.
#[derive(Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CoprocessorPowerEnableDefinition {
    expander: CoprocessorPowerExpander,
    i2c: CoprocessorPowerBusDefinition,
    address: u8,
    pin: u8,
}

impl CoprocessorPowerEnableDefinition {
    /// Returns the expander part.
    #[must_use]
    pub const fn expander(&self) -> CoprocessorPowerExpander {
        self.expander
    }

    /// Returns the chip's I2C controller the expander is on.
    #[must_use]
    pub fn i2c_peripheral(&self) -> &str {
        &self.i2c.peripheral
    }

    /// Returns the I2C clock pin.
    #[must_use]
    pub fn scl(&self) -> &str {
        &self.i2c.scl
    }

    /// Returns the I2C data pin.
    #[must_use]
    pub fn sda(&self) -> &str {
        &self.i2c.sda
    }

    /// Returns the expander's 7-bit I2C address.
    #[must_use]
    pub const fn address(&self) -> u8 {
        self.address
    }

    /// Returns the expander output driven high to power the co-processor.
    #[must_use]
    pub const fn pin(&self) -> u8 {
        self.pin
    }
}

/// I/O expanders a co-processor's power switch may hang off.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum CoprocessorPowerExpander {
    /// Diodes PI4IOE5V6408 eight-bit I2C expander.
    Pi4ioe5v6408,
}

/// The I2C bus an expander is on, by chip-native names.
#[derive(Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct CoprocessorPowerBusDefinition {
    peripheral: String,
    scl: String,
    sda: String,
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
    /// Sixteen-data-line DDR bus, as in the ESP32-P4's in-package PSRAM.
    Hex,
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
    /// A three-wire SPI device must name its one bidirectional line as `mosi`.
    #[error(
        "Board SPI resource `{resource}` must declare `three-wire` with its shared data line as `mosi`, no `miso`, and `read-frequency-hz` only when three-wire"
    )]
    InvalidThreeWireSpi {
        /// Board-level SPI resource name.
        resource: String,
    },
    /// A DSI host declared no lanes, no PHY supply, or an unsupported lane count.
    #[error("Board DSI host `{resource}` has an invalid lane or PHY-power configuration")]
    InvalidDsiHost {
        /// Board-level DSI host resource name.
        resource: String,
    },
    /// The Board's wired Ethernet declaration is inconsistent.
    #[error("Board Ethernet is invalid: {reason}")]
    InvalidEthernet {
        /// What is wrong with the declaration.
        reason: &'static str,
    },
    /// The Board's Wi-Fi co-processor declaration is inconsistent.
    #[error("Board Wi-Fi co-processor is invalid: {reason}")]
    InvalidWifiCoprocessor {
        /// What is wrong with the declaration.
        reason: &'static str,
    },
    /// An SDMMC slot has an unsupported width or no power supply.
    #[error("Board SDMMC device `{resource}` has an invalid bus or power configuration")]
    InvalidSdmmcDevice {
        /// Board-level SDMMC resource name.
        resource: String,
    },
    /// An SDMMC device claims an SD host slot something else is wired to.
    #[error("Board SDMMC device `{resource}` claims SD host slot {slot}, which {user} uses")]
    SdmmcSlotInUse {
        /// Board-level SDMMC resource name.
        resource: String,
        /// The SD host slot both claim.
        slot: u8,
        /// What already holds the slot.
        user: String,
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
    /// SPI devices on one controller disagree on the shared bus lines.
    #[error(
        "Board SPI devices `{resource}` and `{other}` share a controller, so they must name the same `sck`, `mosi`, and `miso` and neither may be three-wire"
    )]
    InconsistentSharedSpi {
        /// Board-level SPI resource name.
        resource: String,
        /// The other device on the same controller.
        other: String,
    },
    /// A PDM microphone and I2S amplifier stream has no direction or half an
    /// amplifier.
    #[error(
        "Board PDM-I2S stream `{resource}` must declare `din`, or `bclk` with `dout`, or all three"
    )]
    InvalidPdmI2sStream {
        /// Board-level stream resource name.
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

/// Directory holding the Plugin profiles Boards start from, relative to the
/// workspace root.
pub const PROFILES_PATH: &str = "boards/profiles";

/// Profile used by a Board that names none.
pub const DEFAULT_PROFILE: &str = "headless";

/// A named Plugin set Boards start from, optionally extending others.
#[derive(Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ProfileDefinition {
    #[serde(default)]
    extends: Extends,
    #[serde(default)]
    enable: Vec<String>,
}

/// `extends`: one profile's name, or a list of them.
#[derive(Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(untagged)]
enum Extends {
    #[default]
    None,
    One(String),
    Many(Vec<String>),
}

impl ProfileDefinition {
    /// Returns the profiles this one extends, in the order written.
    #[must_use]
    pub fn extends(&self) -> &[String] {
        match &self.extends {
            Extends::None => &[],
            Extends::One(name) => core::slice::from_ref(name),
            Extends::Many(names) => names,
        }
    }

    /// Returns the Plugin identities this profile enables on top of the ones
    /// it extends.
    #[must_use]
    pub fn enable(&self) -> &[String] {
        &self.enable
    }
}

/// Parses one Plugin profile document.
///
/// # Errors
///
/// Returns an error when the YAML does not match the profile schema.
pub fn parse_profile(yaml: &str) -> Result<ProfileDefinition, ConfigError> {
    let mut documents = yaml_peg::serde::from_str::<ProfileDefinition>(yaml)
        .map_err(|error| ConfigError::Yaml(error.to_string()))?;
    if documents.len() != 1 {
        return Err(ConfigError::DocumentCount(documents.len()));
    }
    let profile = documents.pop().ok_or(ConfigError::DocumentCount(0))?;
    for parent in profile.extends() {
        validate_resource_name("profile", parent)?;
    }
    Ok(profile)
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
            ExternalMemoryInterfaceDefinition::Hex => {
                "::barracuda_board::ExternalMemoryInterface::Hex"
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
