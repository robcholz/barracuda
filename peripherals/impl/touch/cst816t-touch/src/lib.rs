//! CST816T single-touch peripheral implementation.

#![no_std]

use core::marker::PhantomData;

use barracuda_driver_cst816t::{Cst816t, Error as Cst816tDriverError};
use barracuda_peripheral::{
    PeripheralImplementation,
    touch::{Touch, TouchDescriptor, TouchFrame, TouchPoint},
};
use embedded_hal::{digital::InputPin, i2c::I2c};

/// Board-owned CST816T address and panel geometry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cst816tConfig {
    address: u8,
    width: u16,
    height: u16,
}

impl Cst816tConfig {
    /// Creates a single-touch configuration.
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
pub struct Cst816tBindings<I2C, INTERRUPT> {
    i2c: I2C,
    interrupt: INTERRUPT,
}

impl<I2C, INTERRUPT> Cst816tBindings<I2C, INTERRUPT> {
    /// Combines the control bus and active-low interrupt pin.
    #[must_use]
    pub const fn new(i2c: I2C, interrupt: INTERRUPT) -> Self {
        Self { i2c, interrupt }
    }
}

/// CST816T initialization failure.
#[derive(Debug)]
pub enum Cst816tInitError<BusError> {
    /// The address or panel geometry is invalid.
    InvalidConfig,
    /// The identity register could not be read.
    Bus(BusError),
    /// The attached device is not a known CST816-family controller.
    UnexpectedDevice,
}

/// CST816T sampling failure.
#[derive(Debug)]
pub enum Cst816tError<BusError> {
    /// A register operation failed.
    Bus(BusError),
    /// The device returned an invalid contact count.
    InvalidReport,
}

/// Initialized CST816T touch interface.
pub struct Cst816tTouch<I2C, INTERRUPT> {
    i2c: I2C,
    _interrupt: INTERRUPT,
    chip: Cst816t,
    descriptor: TouchDescriptor,
}

/// Static factory used by generated Board composition.
pub struct Cst816tTouchImplementation<I2C, INTERRUPT>(PhantomData<fn() -> (I2C, INTERRUPT)>);

impl<I2C, INTERRUPT> PeripheralImplementation for Cst816tTouchImplementation<I2C, INTERRUPT>
where
    I2C: I2c + 'static,
    INTERRUPT: InputPin + 'static,
{
    type Bindings = Cst816tBindings<I2C, INTERRUPT>;
    type Config = Cst816tConfig;
    type Peripheral = Cst816tTouch<I2C, INTERRUPT>;
    type Error = Cst816tInitError<I2C::Error>;

    async fn initialize(
        mut bindings: Self::Bindings,
        config: Self::Config,
    ) -> Result<Self::Peripheral, Self::Error> {
        let chip = Cst816t::new(config.address).ok_or(Cst816tInitError::InvalidConfig)?;
        if config.width == 0 || config.height == 0 {
            return Err(Cst816tInitError::InvalidConfig);
        }
        if !chip
            .is_present(&mut bindings.i2c)
            .map_err(Cst816tInitError::Bus)?
        {
            return Err(Cst816tInitError::UnexpectedDevice);
        }
        Ok(Cst816tTouch {
            i2c: bindings.i2c,
            _interrupt: bindings.interrupt,
            chip,
            descriptor: TouchDescriptor {
                width: config.width,
                height: config.height,
                max_points: 1,
            },
        })
    }
}

impl<I2C, INTERRUPT> Touch for Cst816tTouch<I2C, INTERRUPT>
where
    I2C: I2c,
{
    type Error = Cst816tError<I2C::Error>;

    fn descriptor(&self) -> TouchDescriptor {
        self.descriptor
    }

    fn read_frame(&mut self) -> Result<TouchFrame, Self::Error> {
        let report = self
            .chip
            .read_report(&mut self.i2c)
            .map_err(|error| match error {
                Cst816tDriverError::Bus(error) => Cst816tError::Bus(error),
                Cst816tDriverError::InvalidReport => Cst816tError::InvalidReport,
            })?;
        let mut frame = TouchFrame::new();
        if report.touched {
            frame.push(TouchPoint {
                id: 0,
                x: report.x,
                y: report.y,
                strength: 1,
            });
        }
        Ok(frame)
    }
}
