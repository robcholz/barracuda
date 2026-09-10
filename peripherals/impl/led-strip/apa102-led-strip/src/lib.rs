//! APA102/SK9822 clocked LED-strip implementation over Barracuda HAL SPI.

#![no_std]

use core::marker::PhantomData;

use barracuda_driver_apa102::{Apa102, ConfigError as DriverConfigError, Error as DriverError};
use barracuda_peripheral::{
    PeripheralImplementation,
    led_strip::{LedStrip, Rgb8},
};
use embedded_hal::{digital::OutputPin, spi::SpiBus};

/// Board-owned APA102 chain configuration.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Apa102Config {
    led_count: usize,
    global_brightness: u8,
}

impl Apa102Config {
    /// Creates configuration for a fixed chain and five-bit global brightness.
    #[must_use]
    pub const fn new(led_count: usize, global_brightness: u8) -> Self {
        Self {
            led_count,
            global_brightness,
        }
    }
}

/// APA102 initialization failure.
#[derive(Debug)]
pub enum Apa102InitError<PowerError> {
    /// A strip must contain at least one LED.
    EmptyStrip,
    /// APA102 global brightness is a five-bit value.
    InvalidBrightness,
    /// The Board's LED power rail could not be enabled.
    Power(PowerError),
}

/// APA102 output failure.
#[derive(Debug)]
pub enum Apa102Error<SpiError> {
    /// More pixels were supplied than physically configured.
    TooManyPixels,
    /// The SPI transfer failed.
    Spi(SpiError),
}

/// Initialized APA102-compatible LED strip.
pub struct Apa102LedStrip<SPI, POWER> {
    driver: Apa102<SPI>,
    _power_enable: POWER,
}

/// Move-only transport and power resources consumed by the strip implementation.
pub struct Apa102Bindings<SPI, POWER> {
    spi: SPI,
    power_enable: POWER,
}

impl<SPI, POWER> Apa102Bindings<SPI, POWER> {
    /// Combines the exclusive SPI bus and active-low LED power control.
    #[must_use]
    pub const fn new(spi: SPI, power_enable: POWER) -> Self {
        Self { spi, power_enable }
    }
}

/// Static factory used by generated Board composition.
pub struct Apa102LedStripImplementation<SPI, POWER>(PhantomData<fn() -> (SPI, POWER)>);

impl<SPI, POWER> PeripheralImplementation for Apa102LedStripImplementation<SPI, POWER>
where
    SPI: SpiBus<u8> + 'static,
    POWER: OutputPin + 'static,
{
    type Bindings = Apa102Bindings<SPI, POWER>;
    type Config = Apa102Config;
    type Peripheral = Apa102LedStrip<SPI, POWER>;
    type Error = Apa102InitError<POWER::Error>;

    async fn initialize(
        mut bindings: Self::Bindings,
        config: Self::Config,
    ) -> Result<Self::Peripheral, Self::Error> {
        let driver = Apa102::new(bindings.spi, config.led_count, config.global_brightness)
            .map_err(|error| match error {
                DriverConfigError::EmptyChain => Apa102InitError::EmptyStrip,
                DriverConfigError::InvalidBrightness => Apa102InitError::InvalidBrightness,
            })?;
        bindings
            .power_enable
            .set_low()
            .map_err(Apa102InitError::Power)?;
        Ok(Apa102LedStrip {
            driver,
            _power_enable: bindings.power_enable,
        })
    }
}

impl<SPI, POWER> LedStrip for Apa102LedStrip<SPI, POWER>
where
    SPI: SpiBus<u8>,
{
    type Error = Apa102Error<SPI::Error>;

    fn len(&self) -> usize {
        self.driver.len()
    }

    fn write(&mut self, pixels: &[Rgb8]) -> Result<(), Self::Error> {
        self.driver
            .write_rgb(
                pixels
                    .iter()
                    .map(|pixel| (pixel.red, pixel.green, pixel.blue)),
            )
            .map_err(|error| match error {
                DriverError::TooManyPixels => Apa102Error::TooManyPixels,
                DriverError::Spi(error) => Apa102Error::Spi(error),
            })
    }
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    extern crate std;

    use super::{Apa102Bindings, Apa102Config, Apa102LedStrip, Apa102LedStripImplementation};
    use barracuda_peripheral::PeripheralImplementation;
    use barracuda_peripheral::led_strip::{LedStrip, Rgb8};
    use core::{
        convert::Infallible,
        sync::atomic::{AtomicBool, Ordering},
    };
    use embassy_futures::block_on;
    use embedded_hal::{
        digital::{ErrorType as DigitalErrorType, OutputPin},
        spi::{ErrorType, SpiBus},
    };

    #[derive(Default)]
    struct RecordingSpi {
        bytes: std::vec::Vec<u8>,
    }

    static POWER_ENABLED: AtomicBool = AtomicBool::new(false);

    struct PowerEnable;
    impl DigitalErrorType for PowerEnable {
        type Error = Infallible;
    }
    impl OutputPin for PowerEnable {
        fn set_low(&mut self) -> Result<(), Self::Error> {
            POWER_ENABLED.store(true, Ordering::Relaxed);
            Ok(())
        }
        fn set_high(&mut self) -> Result<(), Self::Error> {
            POWER_ENABLED.store(false, Ordering::Relaxed);
            Ok(())
        }
    }
    impl ErrorType for RecordingSpi {
        type Error = Infallible;
    }
    impl SpiBus<u8> for RecordingSpi {
        fn read(&mut self, _: &mut [u8]) -> Result<(), Self::Error> {
            Ok(())
        }
        fn write(&mut self, words: &[u8]) -> Result<(), Self::Error> {
            self.bytes.extend(words);
            Ok(())
        }
        fn transfer(&mut self, _: &mut [u8], _: &[u8]) -> Result<(), Self::Error> {
            Ok(())
        }
        fn transfer_in_place(&mut self, _: &mut [u8]) -> Result<(), Self::Error> {
            Ok(())
        }
        fn flush(&mut self) -> Result<(), Self::Error> {
            Ok(())
        }
    }

    #[test]
    fn writes_start_pixels_and_end_clock() {
        let mut strip = Apa102LedStrip {
            driver: barracuda_driver_apa102::Apa102::new(RecordingSpi::default(), 1, 7)
                .expect("valid chain"),
            _power_enable: PowerEnable,
        };
        strip.write(&[Rgb8::new(1, 2, 3)]).expect("write strip");
        let spi = strip.driver.into_inner();
        assert_eq!(spi.bytes, [0, 0, 0, 0, 0xe7, 3, 2, 1, 0xff]);
    }

    #[test]
    fn initialization_enables_the_board_power_rail() {
        POWER_ENABLED.store(false, Ordering::Relaxed);
        let bindings = Apa102Bindings::new(RecordingSpi::default(), PowerEnable);
        let _strip = block_on(Apa102LedStripImplementation::initialize(
            bindings,
            Apa102Config::new(1, 7),
        ))
        .expect("initialize strip");
        assert!(POWER_ENABLED.load(Ordering::Relaxed));
    }
}
