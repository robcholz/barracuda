//! M5Stack AtomS3R display implementation for Barracuda's stable display API.
//!
//! The AtomS3R carries the AtomS3 panel family (GC9107 or ST7735S, detected
//! at start-up by `atom-s3-display`) but lights it from the white channel of
//! a TI LP5562 LED driver on the internal I2C bus instead of a GPIO. The
//! LP5562 bring-up follows M5GFX's `Light_M5StackAtomS3R`: enable the chip,
//! select its internal clock, place the outputs under register control, and
//! set the white channel's PWM duty as the brightness.

#![no_std]

use barracuda_atom_s3_display::{
    AtomS3Display, AtomS3DisplayConfig, Backlight, FactoryTypes, InitErrorFor, initialize,
};
use barracuda_driver_lp5562::{Channel, Error as Lp5562Error, Lp5562, Options};
use barracuda_peripheral::{PeripheralImplementation, display::DisplayOrientation};
use embedded_hal::{delay::DelayNs, digital::OutputPin, i2c::I2c, spi::SpiDevice};

pub use barracuda_atom_s3_display::{AtomS3DisplayBindings, take_transfer_buffer};

/// Board options of the AtomS3R display.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AtomS3rDisplayConfig {
    backlight_address: u8,
    display: AtomS3DisplayConfig,
}

impl AtomS3rDisplayConfig {
    /// Creates configuration from the LP5562's I2C address and the initial
    /// orientation.
    #[must_use]
    pub const fn new(backlight_address: u8, orientation: DisplayOrientation) -> Self {
        Self {
            backlight_address,
            display: AtomS3DisplayConfig::new(orientation),
        }
    }
}

/// The LP5562's white channel as the panel backlight.
pub struct Lp5562Backlight<I2C> {
    chip: Lp5562<I2C>,
}

impl<I2C: I2c> Backlight for Lp5562Backlight<I2C> {
    type Error = Lp5562Error<I2C::Error>;

    fn set_brightness(&mut self, brightness: u8) -> Result<(), Self::Error> {
        self.chip.set_pwm(Channel::White, brightness)
    }
}

/// Initialization failure.
#[derive(Debug)]
pub enum AtomS3rInitError<PanelError, BacklightError> {
    /// The LP5562 could not be addressed or enabled.
    Backlight(BacklightError),
    /// The panel failed to initialize.
    Panel(PanelError),
}

/// Initialized AtomS3R display.
pub type AtomS3rDisplay<SPI, DC, RST, I2C, DELAY> =
    AtomS3Display<SPI, DC, RST, Lp5562Backlight<I2C>, DELAY>;

/// Static factory used by generated Board composition.
pub struct AtomS3rDisplayImplementation<SPI, DC, RST, I2C, DELAY>(
    FactoryTypes<SPI, DC, RST, I2C, DELAY>,
);

impl<SPI, DC, RST, I2C, DELAY> PeripheralImplementation
    for AtomS3rDisplayImplementation<SPI, DC, RST, I2C, DELAY>
where
    SPI: SpiDevice + 'static,
    DC: OutputPin + 'static,
    RST: OutputPin + 'static,
    I2C: I2c + 'static,
    DELAY: DelayNs + 'static,
{
    type Bindings = AtomS3DisplayBindings<SPI, DC, RST, I2C, DELAY>;
    type Config = AtomS3rDisplayConfig;
    type Peripheral = AtomS3rDisplay<SPI, DC, RST, I2C, DELAY>;
    type Error =
        AtomS3rInitError<InitErrorFor<SPI, DC, RST, Lp5562Backlight<I2C>>, Lp5562Error<I2C::Error>>;

    async fn initialize(
        bindings: Self::Bindings,
        config: Self::Config,
    ) -> Result<Self::Peripheral, Self::Error> {
        let bindings = bindings
            .try_map_backlight(|i2c, delay| {
                let mut chip = Lp5562::new(i2c, config.backlight_address)?;
                chip.enable(Options::default(), delay)?;
                chip.set_pwm(Channel::White, 0)?;
                Ok(Lp5562Backlight { chip })
            })
            .map_err(AtomS3rInitError::Backlight)?;
        initialize(bindings, config.display).map_err(AtomS3rInitError::Panel)
    }
}
