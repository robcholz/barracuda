//! Hynitron CST3530 multi-touch peripheral implementation.
//!
//! The M5Stack CoreP4X carries a CST3530 at 58h on its internal bus (M5GFX
//! `Touch_CST3530`). Its reset is G8 of the M5IOE1 expander, which the
//! display implementation drives high with the panel's other lines, as
//! M5GFX does. M5GFX mounts the panel and the touch sensor both half a turn
//! from their scan order, so the controller's coordinates are already those
//! the display draws in.

#![no_std]

use core::marker::PhantomData;

use barracuda_driver_cst3530::{Cst3530, MAX_POINTS};
use barracuda_peripheral::{
    PeripheralImplementation,
    touch::{Touch, TouchDescriptor, TouchFrame, TouchPoint},
};
use embedded_hal::{delay::DelayNs, i2c::I2c};

/// Start-up attempts, `PROBE_INTERVAL_MS` apart, while the controller leaves
/// reset.
const PROBE_ATTEMPTS: u32 = 10;
const PROBE_INTERVAL_MS: u32 = 50;

/// Board-owned address and reported span.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cst3530Config {
    address: u8,
    width: u16,
    height: u16,
}

impl Cst3530Config {
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
pub struct Cst3530Bindings<I2C, DELAY> {
    i2c: I2C,
    delay: DELAY,
}

impl<I2C, DELAY> Cst3530Bindings<I2C, DELAY> {
    /// Combines the shared I2C device view and the start-up delay.
    #[must_use]
    pub const fn new(i2c: I2C, delay: DELAY) -> Self {
        Self { i2c, delay }
    }
}

/// CST3530 initialization failure.
#[derive(Debug)]
pub enum Cst3530InitError<BusError> {
    /// The address or span is invalid.
    InvalidConfig,
    /// The controller did not acknowledge its start-up commands.
    Bus(BusError),
}

/// Initialized CST3530 touch interface.
pub struct Cst3530Touch<I2C> {
    i2c: I2C,
    chip: Cst3530,
    descriptor: TouchDescriptor,
}

/// Static factory used by generated Board composition.
pub struct Cst3530TouchImplementation<I2C, DELAY>(PhantomData<fn() -> (I2C, DELAY)>);

impl<I2C, DELAY> PeripheralImplementation for Cst3530TouchImplementation<I2C, DELAY>
where
    I2C: I2c + 'static,
    DELAY: DelayNs + 'static,
{
    type Bindings = Cst3530Bindings<I2C, DELAY>;
    type Config = Cst3530Config;
    type Peripheral = Cst3530Touch<I2C>;
    type Error = Cst3530InitError<I2C::Error>;

    async fn initialize(
        mut bindings: Self::Bindings,
        config: Self::Config,
    ) -> Result<Self::Peripheral, Self::Error> {
        let chip = Cst3530::new(config.address).ok_or(Cst3530InitError::InvalidConfig)?;
        if config.width == 0 || config.height == 0 {
            return Err(Cst3530InitError::InvalidConfig);
        }
        let mut attempt = 1;
        loop {
            match chip.initialize(&mut bindings.i2c, &mut bindings.delay) {
                Ok(()) => break,
                Err(_) if attempt < PROBE_ATTEMPTS => {
                    attempt += 1;
                    bindings.delay.delay_ms(PROBE_INTERVAL_MS);
                }
                Err(error) => return Err(Cst3530InitError::Bus(error)),
            }
        }
        Ok(Cst3530Touch {
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

impl<I2C> Touch for Cst3530Touch<I2C>
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
