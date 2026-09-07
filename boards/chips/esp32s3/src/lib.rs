//! Chip-owned translation from ESP32-S3 resources to stable Board binding
//! categories.

#![no_std]

#[cfg(target_arch = "xtensa")]
mod chip {
    use barracuda_board_hal::DigitalLevel;
    use embedded_hal_bus::spi::ExclusiveDevice;
    use esp_hal::{
        Blocking,
        delay::Delay,
        gpio::{
            Input, InputConfig, InputPin, Level, Output, OutputConfig, OutputPin,
            interconnect::PeripheralOutput,
        },
        spi::master::{Config, ConfigError, Instance, Spi},
        time::Rate,
    };

    /// ESP32-S3 peripheral token vocabulary used by generated binding structs.
    pub use esp_hal::peripherals;
    /// SPI configuration failure surfaced by generated Board initialization.
    pub use esp_hal::spi::master::ConfigError as SpiConfigError;

    /// Type-erased push-pull output consumed by digital-output Drivers.
    pub type DigitalOutput = Output<'static>;
    /// Type-erased digital input consumed by input-bound Drivers.
    pub type DigitalInput = Input<'static>;

    /// Blocking SPI bus used to construct statically selected devices.
    pub type SpiBus = Spi<'static, Blocking>;

    /// Standards-compliant single-owner SPI device with GPIO chip-select.
    pub type SpiDevice = ExclusiveDevice<SpiBus, DigitalOutput, Delay>;

    /// Converts one ESP32-S3 pin token into the canonical Driver output.
    #[must_use]
    pub fn digital_output(pin: impl OutputPin + 'static, initial: DigitalLevel) -> DigitalOutput {
        let level = match initial {
            DigitalLevel::Low => Level::Low,
            DigitalLevel::High => Level::High,
        };
        Output::new(pin, level, OutputConfig::default())
    }

    /// Converts one ESP32-S3 pin token into the canonical Driver input.
    #[must_use]
    pub fn digital_input(pin: impl InputPin + 'static) -> DigitalInput {
        Input::new(pin, InputConfig::default())
    }

    /// Constructs one selected SPI device from Board-owned controller and pin
    /// tokens. Protocol Drivers receive only the resulting `embedded-hal`
    /// device and cannot depend on `esp-hal`.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigError`] when the requested frequency cannot be produced.
    pub fn spi_device(
        spi: impl Instance + 'static,
        sck: impl PeripheralOutput<'static> + 'static,
        mosi: impl PeripheralOutput<'static> + 'static,
        chip_select: impl OutputPin + 'static,
        frequency_hz: u32,
    ) -> Result<SpiDevice, ConfigError> {
        let bus = Spi::new(
            spi,
            Config::default().with_frequency(Rate::from_hz(frequency_hz)),
        )?
        .with_sck(sck)
        .with_mosi(mosi);
        let chip_select = digital_output(chip_select, DigitalLevel::High);
        let device = match ExclusiveDevice::new(bus, chip_select, Delay::new()) {
            Ok(device) => device,
            Err(never) => match never {},
        };
        Ok(device)
    }

    /// Delay provider used during Driver initialization and SPI transactions.
    pub type DriverDelay = Delay;

    /// Takes the statically allocated transfer buffer for the Board's primary
    /// direct-panel display.
    ///
    /// The generated HAL constructs the primary display exactly once, so this
    /// move-only allocation cannot alias.
    #[must_use]
    pub fn take_primary_display_buffer() -> &'static mut [u8; 512] {
        static BUFFER: static_cell::StaticCell<[u8; 512]> = static_cell::StaticCell::new();
        BUFFER.init([0; 512])
    }
}

#[cfg(target_arch = "xtensa")]
pub use chip::*;
