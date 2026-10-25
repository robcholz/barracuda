//! The M5Stack PowerHub's eight LEDs, driven by its power co-processor.
//!
//! Start-up follows M5Unified `LED_PowerHub_Class::begin`: the LED supply
//! on (register 00h), then the brightness of every LED (80h-87h). It then
//! turns every LED off. A write sends all eight colors at once (60h-7Fh);
//! the co-processor clocks them out to the WS2812C chain itself.

#![no_std]

use core::marker::PhantomData;

use barracuda_driver_m5powerhub::{LED_COUNT, M5PowerHub, PowerOutput};
use barracuda_peripheral::{
    PeripheralImplementation,
    led_strip::{LedStrip, Rgb8},
};
use embedded_hal::i2c::I2c;

/// Board-owned co-processor address and LED brightness.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PowerHubLedStripConfig {
    address: u8,
    brightness: u8,
}

impl PowerHubLedStripConfig {
    /// Creates configuration for the co-processor at `address`.
    #[must_use]
    pub const fn new(address: u8, brightness: u8) -> Self {
        Self {
            address,
            brightness,
        }
    }
}

/// LED failure.
#[derive(Debug, PartialEq, Eq)]
pub enum PowerHubLedStripError<E> {
    /// The address is not a seven-bit address.
    InvalidAddress(u8),
    /// The co-processor could not be written.
    Bus(E),
}

/// Initialized LEDs.
pub struct PowerHubLedStrip<I2C> {
    i2c: I2C,
    hub: M5PowerHub,
}

/// Static factory used by generated Board composition.
pub struct PowerHubLedStripImplementation<I2C>(PhantomData<fn() -> I2C>);

impl<I2C> PeripheralImplementation for PowerHubLedStripImplementation<I2C>
where
    I2C: I2c + 'static,
{
    type Bindings = I2C;
    type Config = PowerHubLedStripConfig;
    type Peripheral = PowerHubLedStrip<I2C>;
    type Error = PowerHubLedStripError<I2C::Error>;

    async fn initialize(i2c: I2C, config: Self::Config) -> Result<Self::Peripheral, Self::Error> {
        let hub = M5PowerHub::new(config.address)
            .ok_or(PowerHubLedStripError::InvalidAddress(config.address))?;
        let mut strip = PowerHubLedStrip { i2c, hub };
        strip
            .hub
            .set_power(&mut strip.i2c, PowerOutput::Leds, true)
            .map_err(PowerHubLedStripError::Bus)?;
        strip
            .hub
            .set_led_brightness(&mut strip.i2c, config.brightness)
            .map_err(PowerHubLedStripError::Bus)?;
        strip.write(&[])?;
        Ok(strip)
    }
}

impl<I2C: I2c> LedStrip for PowerHubLedStrip<I2C> {
    type Error = PowerHubLedStripError<I2C::Error>;

    fn len(&self) -> usize {
        LED_COUNT
    }

    fn write(&mut self, pixels: &[Rgb8]) -> Result<(), Self::Error> {
        let mut colors = [[0u8; 3]; LED_COUNT];
        for (color, pixel) in colors.iter_mut().zip(pixels) {
            *color = [pixel.red, pixel.green, pixel.blue];
        }
        self.hub
            .write_leds(&mut self.i2c, &colors)
            .map_err(PowerHubLedStripError::Bus)
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::arithmetic_side_effects)]
mod tests {
    extern crate std;

    use core::convert::Infallible;

    use barracuda_peripheral::{
        PeripheralImplementation,
        led_strip::{LedStrip, Rgb8},
    };
    use embassy_futures::block_on;
    use embedded_hal::i2c::{ErrorType, I2c, Operation};

    use super::{PowerHubLedStripConfig, PowerHubLedStripImplementation};

    struct Registers([u8; 256]);

    impl ErrorType for Registers {
        type Error = Infallible;
    }

    impl I2c for Registers {
        fn transaction(
            &mut self,
            _address: u8,
            operations: &mut [Operation<'_>],
        ) -> Result<(), Self::Error> {
            for operation in operations {
                if let Operation::Write(bytes) = operation {
                    let register = usize::from(bytes[0]);
                    for (offset, value) in bytes[1..].iter().enumerate() {
                        self.0[register + offset] = *value;
                    }
                }
            }
            Ok(())
        }
    }

    #[test]
    fn powers_the_leds_sets_brightness_and_writes_blue_first() {
        let mut registers = Registers([0xaa; 256]);
        registers.0[0] = 0;
        let mut strip = block_on(PowerHubLedStripImplementation::initialize(
            registers,
            PowerHubLedStripConfig::new(0x50, 63),
        ))
        .expect("start");
        assert_eq!(strip.i2c.0[0], 1);
        assert_eq!(strip.i2c.0[0x80..0x88], [63; 8]);
        assert_eq!(strip.i2c.0[0x60..0x80], [0; 32]);
        strip
            .write(&[Rgb8::new(10, 20, 30), Rgb8::new(1, 2, 3)])
            .expect("write");
        assert_eq!(strip.i2c.0[0x60..0x68], [30, 20, 10, 0, 3, 2, 1, 0]);
        assert_eq!(strip.i2c.0[0x68..0x80], [0; 24]);
        assert_eq!(strip.len(), 8);
    }
}
