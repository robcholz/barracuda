//! WS2812/SK6812 LED-strip implementation using a 2.4 MHz embedded-hal SPI bus.

#![no_std]

use core::marker::PhantomData;

use barracuda_driver_ws2812_spi::{
    ColorOrder as DriverColorOrder, Error as DriverError, Ws2812Spi,
};
use barracuda_peripheral::{
    PeripheralImplementation,
    led_strip::{LedStrip, Rgb8},
};
use embedded_hal::spi::SpiBus;

/// Component order shifted into the one-wire LED chain.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ColorOrder {
    /// Green, red, then blue; used by WS2812 and most SK6812 RGB parts.
    #[default]
    Grb,
    /// Red, green, then blue.
    Rgb,
}

/// Board-owned LED-strip configuration.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Ws2812Config {
    led_count: usize,
    color_order: ColorOrder,
}

impl Ws2812Config {
    /// Creates configuration for one fixed Board LED chain.
    #[must_use]
    pub const fn new(led_count: usize, color_order: ColorOrder) -> Self {
        Self {
            led_count,
            color_order,
        }
    }
}

/// WS2812 initialization failure.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ws2812InitError {
    /// A strip must contain at least one LED.
    EmptyStrip,
}

/// WS2812 output failure.
#[derive(Debug)]
pub enum Ws2812Error<SpiError> {
    /// More pixels were supplied than physically configured.
    TooManyPixels,
    /// The SPI waveform transfer failed.
    Spi(SpiError),
}

/// Initialized WS2812-compatible LED strip.
pub struct Ws2812SpiLedStrip<SPI> {
    driver: Ws2812Spi<SPI>,
}

/// Static factory used by generated Board composition.
pub struct Ws2812SpiLedStripImplementation<SPI>(PhantomData<fn() -> SPI>);

impl<SPI> PeripheralImplementation for Ws2812SpiLedStripImplementation<SPI>
where
    SPI: SpiBus<u8> + 'static,
{
    type Bindings = SPI;
    type Config = Ws2812Config;
    type Peripheral = Ws2812SpiLedStrip<SPI>;
    type Error = Ws2812InitError;

    async fn initialize(spi: SPI, config: Self::Config) -> Result<Self::Peripheral, Self::Error> {
        let color_order = match config.color_order {
            ColorOrder::Grb => DriverColorOrder::Grb,
            ColorOrder::Rgb => DriverColorOrder::Rgb,
        };
        let driver = Ws2812Spi::new(spi, config.led_count, color_order)
            .map_err(|_| Ws2812InitError::EmptyStrip)?;
        Ok(Ws2812SpiLedStrip { driver })
    }
}

impl<SPI> Ws2812SpiLedStrip<SPI> {
    /// Returns the underlying SPI bus to its owner.
    #[must_use]
    pub fn into_inner(self) -> SPI {
        self.driver.into_inner()
    }
}

impl<SPI> LedStrip for Ws2812SpiLedStrip<SPI>
where
    SPI: SpiBus<u8>,
{
    type Error = Ws2812Error<SPI::Error>;

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
                DriverError::TooManyPixels => Ws2812Error::TooManyPixels,
                DriverError::Spi(error) => Ws2812Error::Spi(error),
            })
    }
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    extern crate std;

    use core::convert::Infallible;

    use barracuda_peripheral::led_strip::{LedStrip, Rgb8};
    use embedded_hal::spi::{ErrorType, Operation, SpiBus};

    use super::Ws2812SpiLedStrip;

    #[derive(Debug, Default)]
    struct RecordingSpi {
        bytes: std::vec::Vec<u8>,
    }

    impl ErrorType for RecordingSpi {
        type Error = Infallible;
    }

    impl SpiBus<u8> for RecordingSpi {
        fn read(&mut self, _words: &mut [u8]) -> Result<(), Self::Error> {
            Ok(())
        }
        fn write(&mut self, words: &[u8]) -> Result<(), Self::Error> {
            self.bytes.extend(words);
            Ok(())
        }
        fn transfer(&mut self, _read: &mut [u8], _write: &[u8]) -> Result<(), Self::Error> {
            Ok(())
        }
        fn transfer_in_place(&mut self, _words: &mut [u8]) -> Result<(), Self::Error> {
            Ok(())
        }
        fn flush(&mut self) -> Result<(), Self::Error> {
            Ok(())
        }
    }

    #[test]
    fn encodes_grb_and_blanks_trailing_pixels() {
        let mut strip = Ws2812SpiLedStrip {
            driver: barracuda_driver_ws2812_spi::Ws2812Spi::new(
                RecordingSpi::default(),
                2,
                barracuda_driver_ws2812_spi::ColorOrder::Grb,
            )
            .expect("valid chain"),
        };
        strip.write(&[Rgb8::new(0xff, 0, 0)]).expect("write strip");
        let spi = strip.into_inner();
        assert_eq!(spi.bytes.len(), 24 + 9 + 9 + 24);
        assert_eq!(&spi.bytes[24..27], &[0x92, 0x49, 0x24]);
        assert_eq!(&spi.bytes[27..30], &[0xdb, 0x6d, 0xb6]);
    }

    #[test]
    fn spi_contract_stays_complete() {
        let _ = core::mem::size_of::<Operation<'static, u8>>();
    }
}
