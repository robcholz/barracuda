//! Chipsemi CHSC6540 two-contact touch peripheral implementation.
//!
//! The M5Stack Tough carries a CHSC6540 at 2Eh on its internal bus (M5GFX
//! `Touch_CHSC6540`). Its reset is the panel's: the AXP192 GPIO4 pulse of the
//! display implementation's power-up resets both (M5GFX `lcd_reset`).

#![no_std]

use core::marker::PhantomData;

use barracuda_driver_chsc6540::Chsc6540;
use barracuda_peripheral::{
    PeripheralImplementation,
    touch::{Touch, TouchDescriptor, TouchFrame, TouchPoint},
};
use embedded_hal::{delay::DelayNs, i2c::I2c};

pub use barracuda_driver_chsc6540::MAX_POINTS;

/// Start-up attempts, `PROBE_INTERVAL_MS` apart, while the controller leaves
/// reset.
const PROBE_ATTEMPTS: u32 = 10;
const PROBE_INTERVAL_MS: u32 = 50;

/// Board-owned address and reported span.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Chsc6540Config {
    address: u8,
    width: u16,
    height: u16,
}

impl Chsc6540Config {
    /// Creates a configuration.
    #[must_use]
    pub const fn new(address: u8, width: u16, height: u16) -> Self {
        Self {
            address,
            width,
            height,
        }
    }
}

/// Move-only resources consumed by the touch implementation.
pub struct Chsc6540Bindings<I2C, DELAY> {
    i2c: I2C,
    delay: DELAY,
}

impl<I2C, DELAY> Chsc6540Bindings<I2C, DELAY> {
    /// Combines the shared I2C device view and the start-up delay.
    #[must_use]
    pub const fn new(i2c: I2C, delay: DELAY) -> Self {
        Self { i2c, delay }
    }
}

/// CHSC6540 initialization failure.
#[derive(Debug)]
pub enum Chsc6540InitError<BusError> {
    /// The address or span is invalid.
    InvalidConfig,
    /// The controller did not acknowledge its start-up write.
    Bus(BusError),
}

/// Initialized CHSC6540 touch interface.
pub struct Chsc6540Touch<I2C> {
    i2c: I2C,
    chip: Chsc6540,
    descriptor: TouchDescriptor,
}

/// Static factory used by generated Board composition.
pub struct Chsc6540TouchImplementation<I2C, DELAY>(PhantomData<fn() -> (I2C, DELAY)>);

impl<I2C, DELAY> PeripheralImplementation for Chsc6540TouchImplementation<I2C, DELAY>
where
    I2C: I2c + 'static,
    DELAY: DelayNs + 'static,
{
    type Bindings = Chsc6540Bindings<I2C, DELAY>;
    type Config = Chsc6540Config;
    type Peripheral = Chsc6540Touch<I2C>;
    type Error = Chsc6540InitError<I2C::Error>;

    async fn initialize(
        mut bindings: Self::Bindings,
        config: Self::Config,
    ) -> Result<Self::Peripheral, Self::Error> {
        let chip = Chsc6540::new(config.address).ok_or(Chsc6540InitError::InvalidConfig)?;
        if config.width == 0 || config.height == 0 {
            return Err(Chsc6540InitError::InvalidConfig);
        }
        let mut attempt = 1;
        loop {
            match chip.initialize(&mut bindings.i2c) {
                Ok(()) => break,
                Err(_) if attempt < PROBE_ATTEMPTS => {
                    attempt += 1;
                    bindings.delay.delay_ms(PROBE_INTERVAL_MS);
                }
                Err(error) => return Err(Chsc6540InitError::Bus(error)),
            }
        }
        Ok(Chsc6540Touch {
            i2c: bindings.i2c,
            chip,
            descriptor: TouchDescriptor {
                width: config.width,
                height: config.height,
                max_points: MAX_POINTS as u8,
            },
        })
    }
}

impl<I2C> Touch for Chsc6540Touch<I2C>
where
    I2C: I2c,
{
    type Error = I2C::Error;

    fn descriptor(&self) -> TouchDescriptor {
        self.descriptor
    }

    fn read_frame(&mut self) -> Result<TouchFrame, Self::Error> {
        let report = self.chip.read_report(&mut self.i2c)?;
        let mut frame = TouchFrame::new();
        for point in report.points() {
            frame.push(TouchPoint {
                id: point.id,
                x: point.x,
                y: point.y,
                strength: 1,
            });
        }
        Ok(frame)
    }
}
