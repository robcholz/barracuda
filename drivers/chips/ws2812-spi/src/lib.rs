//! WS2812/SK6812 waveform encoder for a 2.4 MHz SPI bus.

#![no_std]

use embedded_hal::spi::SpiBus;

const RESET_BYTES: [u8; 24] = [0; 24];

/// Wire component order.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ColorOrder {
    /// Green, red, blue.
    #[default]
    Grb,
    /// Red, green, blue.
    Rgb,
}

/// WS2812 protocol or transport failure.
#[derive(Debug)]
pub enum Error<E> {
    /// More RGB tuples were supplied than the configured chain length.
    TooManyPixels,
    /// The underlying SPI operation failed.
    Spi(E),
}

/// An initialized encoded-SPI WS2812-compatible chain.
pub struct Ws2812Spi<SPI> {
    spi: SPI,
    led_count: usize,
    color_order: ColorOrder,
}

impl<SPI> Ws2812Spi<SPI> {
    /// Creates a chain, or returns the SPI bus when the length is zero.
    pub fn new(spi: SPI, led_count: usize, color_order: ColorOrder) -> Result<Self, SPI> {
        if led_count == 0 {
            Err(spi)
        } else {
            Ok(Self {
                spi,
                led_count,
                color_order,
            })
        }
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

impl<SPI: SpiBus<u8>> Ws2812Spi<SPI> {
    /// Writes RGB tuples and blanks any trailing LEDs.
    pub fn write_rgb<I>(&mut self, pixels: I) -> Result<(), Error<SPI::Error>>
    where
        I: ExactSizeIterator<Item = (u8, u8, u8)>,
    {
        let pixel_count = pixels.len();
        if pixel_count > self.led_count {
            return Err(Error::TooManyPixels);
        }
        self.spi.write(&RESET_BYTES).map_err(Error::Spi)?;
        for (red, green, blue) in pixels {
            self.write_pixel(red, green, blue)?;
        }
        for _ in pixel_count..self.led_count {
            self.write_pixel(0, 0, 0)?;
        }
        self.spi.write(&RESET_BYTES).map_err(Error::Spi)
    }

    fn write_pixel(&mut self, red: u8, green: u8, blue: u8) -> Result<(), Error<SPI::Error>> {
        let components = match self.color_order {
            ColorOrder::Grb => [green, red, blue],
            ColorOrder::Rgb => [red, green, blue],
        };
        let mut encoded = [0_u8; 9];
        for (component_index, component) in components.into_iter().enumerate() {
            let start = component_index.saturating_mul(3);
            encode_component(component, &mut encoded[start..start.saturating_add(3)]);
        }
        self.spi.write(&encoded).map_err(Error::Spi)
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
