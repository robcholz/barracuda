//! WS2812/SK6812 LED-strip Driver using a 2.4 MHz embedded-hal SPI bus.

#![no_std]

use core::marker::PhantomData;

use barracuda_driver::{
    PeripheralDriver,
    led_strip::{LedStrip, Rgb8},
};
use embedded_hal::spi::SpiBus;

const RESET_BYTES: [u8; 24] = [0; 24];

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
    spi: SPI,
    config: Ws2812Config,
}

/// Static factory used by generated Board composition.
pub struct Ws2812SpiLedStripDriver<SPI>(PhantomData<fn() -> SPI>);

impl<SPI> PeripheralDriver for Ws2812SpiLedStripDriver<SPI>
where
    SPI: SpiBus<u8> + 'static,
{
    type Bindings = SPI;
    type Config = Ws2812Config;
    type Capability = Ws2812SpiLedStrip<SPI>;
    type Error = Ws2812InitError;

    async fn initialize(spi: SPI, config: Self::Config) -> Result<Self::Capability, Self::Error> {
        if config.led_count == 0 {
            return Err(Ws2812InitError::EmptyStrip);
        }
        Ok(Ws2812SpiLedStrip { spi, config })
    }
}

impl<SPI> Ws2812SpiLedStrip<SPI> {
    /// Returns the underlying SPI bus to its owner.
    #[must_use]
    pub fn into_inner(self) -> SPI {
        self.spi
    }
}

impl<SPI> LedStrip for Ws2812SpiLedStrip<SPI>
where
    SPI: SpiBus<u8>,
{
    type Error = Ws2812Error<SPI::Error>;

    fn len(&self) -> usize {
        self.config.led_count
    }

    fn write(&mut self, pixels: &[Rgb8]) -> Result<(), Self::Error> {
        if pixels.len() > self.config.led_count {
            return Err(Ws2812Error::TooManyPixels);
        }
        self.spi.write(&RESET_BYTES).map_err(Ws2812Error::Spi)?;
        for pixel in pixels {
            self.write_pixel(*pixel)?;
        }
        for _ in pixels.len()..self.config.led_count {
            self.write_pixel(Rgb8::default())?;
        }
        self.spi.write(&RESET_BYTES).map_err(Ws2812Error::Spi)
    }
}

impl<SPI> Ws2812SpiLedStrip<SPI>
where
    SPI: SpiBus<u8>,
{
    fn write_pixel(&mut self, pixel: Rgb8) -> Result<(), Ws2812Error<SPI::Error>> {
        let components = match self.config.color_order {
            ColorOrder::Grb => [pixel.green, pixel.red, pixel.blue],
            ColorOrder::Rgb => [pixel.red, pixel.green, pixel.blue],
        };
        let mut encoded = [0_u8; 9];
        for (component_index, component) in components.into_iter().enumerate() {
            encode_component(
                component,
                &mut encoded[component_index * 3..component_index * 3 + 3],
            );
        }
        self.spi.write(&encoded).map_err(Ws2812Error::Spi)
    }
}

fn encode_component(component: u8, output: &mut [u8]) {
    let mut waveform = 0_u32;
    for bit in (0..8).rev() {
        waveform <<= 3;
        waveform |= if component & (1 << bit) == 0 {
            0b100
        } else {
            0b110
        };
    }
    output.copy_from_slice(&waveform.to_be_bytes()[1..]);
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    extern crate std;

    use core::convert::Infallible;

    use barracuda_driver::led_strip::{LedStrip, Rgb8};
    use embedded_hal::spi::{ErrorType, Operation, SpiBus};

    use super::{ColorOrder, Ws2812Config, Ws2812SpiLedStrip};

    #[derive(Default)]
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
            spi: RecordingSpi::default(),
            config: Ws2812Config::new(2, ColorOrder::Grb),
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
