//! APA102/SK9822 clocked LED-chain protocol driver.

#![no_std]

use embedded_hal::spi::SpiBus;

/// Invalid physical chain configuration.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConfigError {
    /// A chain must contain at least one LED.
    EmptyChain,
    /// Global brightness is a five-bit field.
    InvalidBrightness,
}

/// APA102 protocol or transport failure.
#[derive(Debug)]
pub enum Error<E> {
    /// More RGB tuples were supplied than the configured chain length.
    TooManyPixels,
    /// The underlying SPI operation failed.
    Spi(E),
}

/// An initialized APA102-compatible chain.
pub struct Apa102<SPI> {
    spi: SPI,
    led_count: usize,
    global_brightness: u8,
}

impl<SPI> Apa102<SPI> {
    /// Validates and creates a physical LED chain.
    pub fn new(spi: SPI, led_count: usize, global_brightness: u8) -> Result<Self, ConfigError> {
        if led_count == 0 {
            return Err(ConfigError::EmptyChain);
        }
        if global_brightness > 31 {
            return Err(ConfigError::InvalidBrightness);
        }
        Ok(Self {
            spi,
            led_count,
            global_brightness,
        })
    }

    /// Returns the number of physical LEDs.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.led_count
    }

    /// Returns whether the physical chain is empty.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.led_count == 0
    }

    /// Returns ownership of the SPI bus.
    #[must_use]
    pub fn into_inner(self) -> SPI {
        self.spi
    }
}

impl<SPI: SpiBus<u8>> Apa102<SPI> {
    /// Writes RGB tuples and blanks any trailing LEDs.
    pub fn write_rgb<I>(&mut self, pixels: I) -> Result<(), Error<SPI::Error>>
    where
        I: ExactSizeIterator<Item = (u8, u8, u8)>,
    {
        let pixel_count = pixels.len();
        if pixel_count > self.led_count {
            return Err(Error::TooManyPixels);
        }
        self.spi.write(&[0; 4]).map_err(Error::Spi)?;
        for (red, green, blue) in pixels {
            self.write_pixel(red, green, blue)?;
        }
        for _ in pixel_count..self.led_count {
            self.write_pixel(0, 0, 0)?;
        }
        for _ in 0..self.led_count.div_ceil(16) {
            self.spi.write(&[0xff]).map_err(Error::Spi)?;
        }
        self.spi.flush().map_err(Error::Spi)
    }

    fn write_pixel(&mut self, red: u8, green: u8, blue: u8) -> Result<(), Error<SPI::Error>> {
        self.spi
            .write(&[0xe0 | self.global_brightness, blue, green, red])
            .map_err(Error::Spi)
    }
}
