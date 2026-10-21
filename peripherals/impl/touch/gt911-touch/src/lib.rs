//! Touch implementation for a Goodix GT911 at either of its two addresses.
//!
//! The GT911 samples its interrupt line while it leaves reset and answers at
//! `14h` or `5Dh` accordingly. Where nothing drives that line at the time,
//! the address is not known in advance, so the implementation reads the
//! product ID at the configured address and then at the other one, as
//! M5GFX's `Touch_GT911::init` and Espressif's ESP-BOX BSP do. It never
//! resets the controller.

#![no_std]

use core::marker::PhantomData;

use barracuda_driver_gt911::{Error as Gt911Error, Gt911};
use barracuda_peripheral::{
    PeripheralImplementation,
    touch::{Touch, TouchDescriptor},
};
use embedded_hal::i2c::I2c;

pub use barracuda_driver_gt911::MAX_POINTS;
pub use barracuda_peripheral::touch::{TouchFrame, TouchPoint};

/// The address with the interrupt line low at reset.
pub const ADDRESS_LOW: u8 = 0x5d;
/// The address with the interrupt line high at reset.
pub const ADDRESS_HIGH: u8 = 0x14;

/// Board-owned address and coordinate span.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Gt911TouchConfig {
    address: u8,
    width: u16,
    height: u16,
}

impl Gt911TouchConfig {
    /// Creates a configuration that tries `address` first.
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
pub struct Gt911TouchBindings<I2C> {
    i2c: I2C,
}

impl<I2C> Gt911TouchBindings<I2C> {
    /// Takes the shared I2C device view.
    #[must_use]
    pub const fn new(i2c: I2C) -> Self {
        Self { i2c }
    }
}

/// Touch initialization failure.
#[derive(Debug)]
pub enum Gt911TouchInitError<BusError> {
    /// The configured address is neither GT911 address.
    InvalidAddress(u8),
    /// No GT911 answered at either address.
    NotFound,
    /// The probe at the second address failed at the bus layer.
    Bus(BusError),
}

/// Touch sampling failure.
#[derive(Debug)]
pub enum Gt911TouchError<BusError> {
    /// A register operation failed.
    Bus(BusError),
    /// The controller returned a malformed report.
    InvalidReport,
}

/// Finds the GT911 at `first` or at the other strap address.
///
/// # Errors
///
/// Returns an invalid-address, not-found, or bus failure.
pub fn find_gt911<I2C: I2c>(
    i2c: &mut I2C,
    first: u8,
) -> Result<Gt911, Gt911TouchInitError<I2C::Error>> {
    let second = match first {
        ADDRESS_LOW => ADDRESS_HIGH,
        ADDRESS_HIGH => ADDRESS_LOW,
        other => return Err(Gt911TouchInitError::InvalidAddress(other)),
    };
    let mut last_error = None;
    for address in [first, second] {
        let chip = Gt911::new(address).ok_or(Gt911TouchInitError::InvalidAddress(address))?;
        match chip.is_present(i2c) {
            Ok(true) => return Ok(chip),
            Ok(false) => {}
            Err(error) => last_error = Some(error),
        }
    }
    Err(last_error.map_or(Gt911TouchInitError::NotFound, Gt911TouchInitError::Bus))
}

/// Reads one GT911 report into a portable frame.
///
/// # Errors
///
/// Returns the bus failure or a malformed-report failure.
pub fn read_gt911_frame<I2C: I2c>(
    chip: &Gt911,
    i2c: &mut I2C,
) -> Result<TouchFrame, Gt911TouchError<I2C::Error>> {
    let report = chip.read_report(i2c).map_err(|error| match error {
        Gt911Error::Bus(error) => Gt911TouchError::Bus(error),
        Gt911Error::InvalidReport => Gt911TouchError::InvalidReport,
    })?;
    let mut frame = TouchFrame::new();
    for point in report.points() {
        frame.push(TouchPoint {
            id: point.id,
            x: point.x,
            y: point.y,
            strength: point.size,
        });
    }
    Ok(frame)
}

/// Initialized GT911 touch input.
pub struct Gt911Touch<I2C> {
    i2c: I2C,
    chip: Gt911,
    descriptor: TouchDescriptor,
}

/// Static factory used by generated Board composition.
pub struct Gt911TouchImplementation<I2C>(PhantomData<I2C>);

impl<I2C> PeripheralImplementation for Gt911TouchImplementation<I2C>
where
    I2C: I2c + 'static,
{
    type Bindings = Gt911TouchBindings<I2C>;
    type Config = Gt911TouchConfig;
    type Peripheral = Gt911Touch<I2C>;
    type Error = Gt911TouchInitError<I2C::Error>;

    async fn initialize(
        mut bindings: Self::Bindings,
        config: Self::Config,
    ) -> Result<Self::Peripheral, Self::Error> {
        let chip = find_gt911(&mut bindings.i2c, config.address)?;
        Ok(Gt911Touch {
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

impl<I2C: I2c> Touch for Gt911Touch<I2C> {
    type Error = Gt911TouchError<I2C::Error>;

    fn descriptor(&self) -> TouchDescriptor {
        self.descriptor
    }

    fn read_frame(&mut self) -> Result<TouchFrame, Self::Error> {
        read_gt911_frame(&self.chip, &mut self.i2c)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::arithmetic_side_effects)]

    use barracuda_peripheral::{PeripheralImplementation, touch::Touch};
    use embassy_futures::block_on;
    use embedded_hal::i2c::{ErrorKind, ErrorType, I2c, Operation};

    use super::{
        Gt911TouchBindings, Gt911TouchConfig, Gt911TouchImplementation, Gt911TouchInitError,
    };

    /// A GT911 at `address`; other addresses do not acknowledge.
    struct Controller {
        address: u8,
        status: u8,
    }

    impl ErrorType for Controller {
        type Error = ErrorKind;
    }

    impl I2c for Controller {
        fn transaction(
            &mut self,
            address: u8,
            operations: &mut [Operation<'_>],
        ) -> Result<(), Self::Error> {
            if address != self.address {
                return Err(ErrorKind::NoAcknowledge(
                    embedded_hal::i2c::NoAcknowledgeSource::Address,
                ));
            }
            let mut register = 0_u16;
            for operation in operations {
                match operation {
                    Operation::Write(bytes) if bytes.len() == 2 => {
                        register = u16::from_be_bytes([bytes[0], bytes[1]]);
                    }
                    Operation::Write(bytes) => {
                        if u16::from_be_bytes([bytes[0], bytes[1]]) == 0x814e {
                            self.status = bytes[2];
                        }
                    }
                    Operation::Read(bytes) => match register {
                        0x8140 => bytes.copy_from_slice(b"911\0"),
                        0x814e => bytes[0] = self.status,
                        // Track 1 at (100, 900), size 30.
                        0x8150 => bytes[..8].copy_from_slice(&[1, 100, 0, 0x84, 0x03, 30, 0, 0]),
                        _ => bytes.fill(0),
                    },
                }
            }
            Ok(())
        }
    }

    #[test]
    fn finds_the_controller_at_its_other_address_and_reads_contacts() {
        // M5Paper: the Board names 14h, and this unit answers at 5Dh.
        let mut touch = block_on(Gt911TouchImplementation::initialize(
            Gt911TouchBindings::new(Controller {
                address: 0x5d,
                status: 0x81,
            }),
            Gt911TouchConfig::new(0x14, 540, 960),
        ))
        .expect("initialize");
        assert_eq!(touch.descriptor().width, 540);
        assert_eq!(touch.descriptor().max_points, 5);
        let frame = touch.read_frame().expect("frame");
        assert_eq!(frame.len(), 1);
        let point = frame.get(0).expect("point");
        assert_eq!(
            (point.id, point.x, point.y, point.strength),
            (1, 100, 900, 30)
        );
        assert_eq!(touch.i2c.status, 0, "report acknowledged");
        assert!(touch.read_frame().expect("frame").is_empty());
    }

    #[test]
    fn reports_a_missing_controller_and_a_foreign_address() {
        let missing = block_on(Gt911TouchImplementation::initialize(
            Gt911TouchBindings::new(Controller {
                address: 0x38,
                status: 0,
            }),
            Gt911TouchConfig::new(0x5d, 320, 240),
        ));
        assert!(matches!(missing, Err(Gt911TouchInitError::Bus(_))));
        let foreign = block_on(Gt911TouchImplementation::initialize(
            Gt911TouchBindings::new(Controller {
                address: 0x38,
                status: 0,
            }),
            Gt911TouchConfig::new(0x38, 320, 240),
        ));
        assert!(matches!(
            foreign,
            Err(Gt911TouchInitError::InvalidAddress(0x38))
        ));
    }
}
