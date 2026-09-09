//! Touch implementation for both shipping revisions of the M5Stack Tab5.

#![no_std]

use core::marker::PhantomData;

use barracuda_driver_gt911::{Error as Gt911Error, Gt911};
use barracuda_driver_pi4ioe5v6408::{Error as ExpanderError, Pi4ioe5v6408};
use barracuda_driver_st712x::St712x;
use barracuda_peripheral::{
    PeripheralImplementation,
    touch::{Touch, TouchDescriptor},
};
use embedded_hal::{delay::DelayNs, i2c::I2c};

pub use barracuda_peripheral::touch::{TouchFrame, TouchPoint};

const CONTROL_EXPANDER_ADDRESS: u8 = 0x43;
const TOUCH_RESET_PIN: u8 = 5;

/// Touch-controller family discovered at runtime.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TouchControllerKind {
    /// Discrete Goodix GT911 used by the original Tab5.
    Gt911,
    /// Sitronix ST7123/ST7121 integrated display-touch controller.
    St712x,
}

/// Board-owned addresses for both possible controller revisions.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Tab5TouchConfig {
    gt911_address: u8,
    st712x_address: u8,
}

impl Tab5TouchConfig {
    /// Creates an automatic-detection configuration.
    #[must_use]
    pub const fn new(gt911_address: u8, st712x_address: u8) -> Self {
        Self {
            gt911_address,
            st712x_address,
        }
    }
}

/// Move-only resources consumed by the touch implementation.
pub struct Tab5TouchBindings<I2C, DELAY> {
    i2c: I2C,
    delay: DELAY,
}

impl<I2C, DELAY> Tab5TouchBindings<I2C, DELAY> {
    /// Combines the shared I2C device view and startup delay.
    #[must_use]
    pub const fn new(i2c: I2C, delay: DELAY) -> Self {
        Self { i2c, delay }
    }
}

/// Touch initialization failure.
#[derive(Debug)]
pub enum Tab5TouchInitError<BusError> {
    /// One or both configured addresses are invalid.
    InvalidAddresses,
    /// Neither supported controller produced a valid identity response.
    UnsupportedController,
    /// The final controller probe failed at the bus layer.
    Bus(BusError),
    /// The private Tab5 touch-reset expander could not be configured.
    Expander(ExpanderError<BusError>),
}

/// Touch sampling failure.
#[derive(Debug)]
pub enum Tab5TouchError<BusError> {
    /// A register operation failed.
    Bus(BusError),
    /// A controller returned a malformed report.
    InvalidReport,
}

enum Controller {
    Gt911(Gt911),
    St712x(St712x),
}

/// Initialized, revision-aware Tab5 touch interface.
pub struct Tab5Touch<I2C> {
    i2c: I2C,
    controller: Controller,
}

/// Static factory used by generated Board composition.
pub struct Tab5TouchImplementation<I2C, DELAY>(PhantomData<I2C>, PhantomData<DELAY>);

impl<I2C, DELAY> PeripheralImplementation for Tab5TouchImplementation<I2C, DELAY>
where
    I2C: I2c + 'static,
    DELAY: DelayNs + 'static,
{
    type Bindings = Tab5TouchBindings<I2C, DELAY>;
    type Config = Tab5TouchConfig;
    type Peripheral = Tab5Touch<I2C>;
    type Error = Tab5TouchInitError<I2C::Error>;

    async fn initialize(
        mut bindings: Self::Bindings,
        config: Self::Config,
    ) -> Result<Self::Peripheral, Self::Error> {
        let gt911 = Gt911::new(config.gt911_address).ok_or(Tab5TouchInitError::InvalidAddresses)?;
        if config.st712x_address > 0x7f || config.gt911_address == config.st712x_address {
            return Err(Tab5TouchInitError::InvalidAddresses);
        }
        Pi4ioe5v6408::new(&mut bindings.i2c, CONTROL_EXPANDER_ADDRESS)
            .map_err(Tab5TouchInitError::Expander)?
            .pulse_reset_low(TOUCH_RESET_PIN, 10, 50, &mut bindings.delay)
            .map_err(Tab5TouchInitError::Expander)?;

        if let Ok(Some(controller)) = St712x::probe(&mut bindings.i2c, config.st712x_address) {
            return Ok(Tab5Touch {
                i2c: bindings.i2c,
                controller: Controller::St712x(controller),
            });
        }
        match gt911.is_present(&mut bindings.i2c) {
            Ok(true) => Ok(Tab5Touch {
                i2c: bindings.i2c,
                controller: Controller::Gt911(gt911),
            }),
            Ok(false) => Err(Tab5TouchInitError::UnsupportedController),
            Err(error) => Err(Tab5TouchInitError::Bus(error)),
        }
    }
}

impl<I2C> Tab5Touch<I2C>
where
    I2C: I2c,
{
    /// Returns the controller selected during automatic probing.
    #[must_use]
    pub const fn controller_kind(&self) -> TouchControllerKind {
        match self.controller {
            Controller::Gt911(_) => TouchControllerKind::Gt911,
            Controller::St712x(_) => TouchControllerKind::St712x,
        }
    }
}

impl<I2C> Touch for Tab5Touch<I2C>
where
    I2C: I2c,
{
    type Error = Tab5TouchError<I2C::Error>;

    fn descriptor(&self) -> TouchDescriptor {
        match &self.controller {
            Controller::Gt911(_) => TouchDescriptor {
                width: 720,
                height: 1280,
                max_points: 5,
            },
            Controller::St712x(controller) => {
                let info = controller.touch_info();
                TouchDescriptor {
                    width: info.width,
                    height: info.height,
                    max_points: info.max_points,
                }
            }
        }
    }

    fn read_frame(&mut self) -> Result<TouchFrame, Self::Error> {
        let mut frame = TouchFrame::new();
        match &self.controller {
            Controller::Gt911(controller) => {
                let report =
                    controller
                        .read_report(&mut self.i2c)
                        .map_err(|error| match error {
                            Gt911Error::Bus(error) => Tab5TouchError::Bus(error),
                            Gt911Error::InvalidReport => Tab5TouchError::InvalidReport,
                        })?;
                for point in report.points() {
                    frame.push(TouchPoint {
                        id: point.id,
                        x: point.x,
                        y: point.y,
                        strength: point.size,
                    });
                }
            }
            Controller::St712x(controller) => {
                let report = controller
                    .read_report(&mut self.i2c)
                    .map_err(Tab5TouchError::Bus)?;
                for point in report.points() {
                    frame.push(TouchPoint {
                        id: point.id,
                        x: point.x,
                        y: point.y,
                        strength: point.strength,
                    });
                }
            }
        }
        Ok(frame)
    }
}
