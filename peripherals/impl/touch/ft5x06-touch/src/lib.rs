//! FocalTech FT5x06-family (FT6336U) multi-touch peripheral implementation.
//!
//! M5Stack's Core2 and CoreS3 carry an FT6336U at 38h on their internal bus
//! (M5GFX `Touch_FT5x06` and `Touch_M5StackCoreS3`). On the CoreS3 its reset
//! is P0_0 of the AW9523B expander at 58h, which this implementation
//! releases; on the Core2 the display implementation's PMIC power-up resets
//! it with the panel.

#![no_std]

use core::marker::PhantomData;

use barracuda_driver_aw9523::{Aw9523, Error as ExpanderError};
use barracuda_driver_ft5x06::{Error as Ft5x06Error, Ft5x06, MAX_POINTS};
use barracuda_peripheral::{
    PeripheralImplementation,
    touch::{Touch, TouchDescriptor, TouchFrame, TouchPoint},
};
use embedded_hal::{delay::DelayNs, i2c::I2c};

/// M5GFX `aw9523_i2c_addr`: the CoreS3 AW9523B.
const CORES3_EXPANDER_ADDRESS: u8 = 0x58;
/// The CoreS3 touch reset, AW9523B P0_0 (Espressif's m5stack_cores3
/// `CORES3_POWER_CTRL_FEATURE_TOUCH` drives it high to enable the touch).
const CORES3_TOUCH_RESET_PIN: u8 = 0;
/// Start-up attempts, `PROBE_INTERVAL_MS` apart, while the controller leaves
/// reset.
const PROBE_ATTEMPTS: u32 = 10;
const PROBE_INTERVAL_MS: u32 = 50;

/// How the controller's reset line is reached.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TouchReset {
    /// The controller leaves reset with the display or at power-on.
    None,
    /// AW9523B P0_0 at 58h, as on the M5Stack CoreS3: M5GFX makes port 0
    /// push-pull and drives the pin high.
    CoreS3,
}

/// Board-owned address, reported span, and reset wiring.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Ft5x06Config {
    address: u8,
    width: u16,
    height: u16,
    max_points: u8,
    reset: TouchReset,
}

impl Ft5x06Config {
    /// Creates a configuration.
    #[must_use]
    pub const fn new(
        address: u8,
        width: u16,
        height: u16,
        max_points: u8,
        reset: TouchReset,
    ) -> Self {
        Self {
            address,
            width,
            height,
            max_points,
            reset,
        }
    }
}

/// Move-only resources consumed by the touch implementation.
pub struct Ft5x06Bindings<I2C, DELAY> {
    i2c: I2C,
    delay: DELAY,
}

impl<I2C, DELAY> Ft5x06Bindings<I2C, DELAY> {
    /// Combines the shared I2C device view and the start-up delay.
    #[must_use]
    pub const fn new(i2c: I2C, delay: DELAY) -> Self {
        Self { i2c, delay }
    }
}

/// FT5x06 initialization failure.
#[derive(Debug)]
pub enum Ft5x06InitError<BusError> {
    /// The address, span, or contact count is invalid.
    InvalidConfig,
    /// The reset expander could not be configured.
    Expander(ExpanderError<BusError>),
    /// The controller did not answer at start-up.
    Bus(BusError),
    /// The controller answered with a zero vendor ID.
    NotPresent,
}

/// Initialized FT5x06-family touch interface.
pub struct Ft5x06Touch<I2C> {
    i2c: I2C,
    chip: Ft5x06,
    descriptor: TouchDescriptor,
}

/// Static factory used by generated Board composition.
pub struct Ft5x06TouchImplementation<I2C, DELAY>(PhantomData<fn() -> (I2C, DELAY)>);

impl<I2C, DELAY> PeripheralImplementation for Ft5x06TouchImplementation<I2C, DELAY>
where
    I2C: I2c + 'static,
    DELAY: DelayNs + 'static,
{
    type Bindings = Ft5x06Bindings<I2C, DELAY>;
    type Config = Ft5x06Config;
    type Peripheral = Ft5x06Touch<I2C>;
    type Error = Ft5x06InitError<I2C::Error>;

    async fn initialize(
        mut bindings: Self::Bindings,
        config: Self::Config,
    ) -> Result<Self::Peripheral, Self::Error> {
        let chip = Ft5x06::new(config.address).ok_or(Ft5x06InitError::InvalidConfig)?;
        if config.width == 0
            || config.height == 0
            || config.max_points == 0
            || usize::from(config.max_points) > MAX_POINTS
        {
            return Err(Ft5x06InitError::InvalidConfig);
        }
        if config.reset == TouchReset::CoreS3 {
            let mut expander = Aw9523::new(&mut bindings.i2c, CORES3_EXPANDER_ADDRESS)
                .map_err(Ft5x06InitError::Expander)?;
            expander
                .set_port0_push_pull(true)
                .map_err(Ft5x06InitError::Expander)?;
            expander
                .configure_output(CORES3_TOUCH_RESET_PIN, true)
                .map_err(Ft5x06InitError::Expander)?;
        }
        let mut attempt = 1;
        loop {
            match chip.initialize(&mut bindings.i2c) {
                Ok(_) => break,
                Err(_) if attempt < PROBE_ATTEMPTS => {
                    attempt += 1;
                    bindings.delay.delay_ms(PROBE_INTERVAL_MS);
                }
                Err(Ft5x06Error::Bus(error)) => return Err(Ft5x06InitError::Bus(error)),
                Err(Ft5x06Error::NotPresent) => return Err(Ft5x06InitError::NotPresent),
            }
        }
        Ok(Ft5x06Touch {
            i2c: bindings.i2c,
            chip,
            descriptor: TouchDescriptor {
                width: config.width,
                height: config.height,
                max_points: config.max_points,
            },
        })
    }
}

impl<I2C> Touch for Ft5x06Touch<I2C>
where
    I2C: I2c,
{
    type Error = I2C::Error;

    fn descriptor(&self) -> TouchDescriptor {
        self.descriptor
    }

    fn read_frame(&mut self) -> Result<TouchFrame, Self::Error> {
        let report = self
            .chip
            .read_report(&mut self.i2c, usize::from(self.descriptor.max_points))?;
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
