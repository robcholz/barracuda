//! Touch implementation for products fitted with either a Goodix GT911 or a
//! Parade TT21100.
//!
//! Espressif's ESP-BOX BSPs (`bsp_touch_new` in esp-bsp's `esp-box-3`) probe
//! the GT911 at `5Dh`, then at `14h`, then the TT21100 at `24h`, and mirror
//! the TT21100's X axis. This implementation probes in the same order and
//! takes the mirroring from the Board.

#![no_std]

use core::marker::PhantomData;

use barracuda_driver_gt911::{Gt911, MAX_POINTS as GT911_MAX_POINTS};
use barracuda_driver_tt21100::{MAX_POINTS as TT21100_MAX_POINTS, Tt21100};
use barracuda_gt911_touch::{Gt911TouchError, Gt911TouchInitError, find_gt911, read_gt911_frame};
use barracuda_peripheral::{
    PeripheralImplementation,
    touch::{Touch, TouchDescriptor, TouchFrame, TouchPoint},
};
use embedded_hal::i2c::I2c;

/// Touch-controller family found at initialization.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TouchControllerKind {
    /// Goodix GT911.
    Gt911,
    /// Parade TT21100.
    Tt21100,
}

/// Board-owned addresses, span, and TT21100 mirroring.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Gt911Tt21100TouchConfig {
    gt911_address: u8,
    tt21100_address: u8,
    width: u16,
    height: u16,
    tt21100_mirror_x: bool,
}

impl Gt911Tt21100TouchConfig {
    /// Creates a detection configuration.
    #[must_use]
    pub const fn new(
        gt911_address: u8,
        tt21100_address: u8,
        width: u16,
        height: u16,
        tt21100_mirror_x: bool,
    ) -> Self {
        Self {
            gt911_address,
            tt21100_address,
            width,
            height,
            tt21100_mirror_x,
        }
    }
}

/// Move-only resources consumed by the touch implementation.
pub struct Gt911Tt21100TouchBindings<I2C> {
    i2c: I2C,
}

impl<I2C> Gt911Tt21100TouchBindings<I2C> {
    /// Takes the shared I2C device view.
    #[must_use]
    pub const fn new(i2c: I2C) -> Self {
        Self { i2c }
    }
}

/// Touch initialization failure.
#[derive(Debug)]
pub enum Gt911Tt21100TouchInitError {
    /// A configured address is not one the controller can take.
    InvalidAddress(u8),
    /// Neither controller answered.
    NotFound,
}

/// Touch sampling failure.
pub type Gt911Tt21100TouchError<BusError> = Gt911TouchError<BusError>;

enum Controller {
    Gt911(Gt911),
    Tt21100(Tt21100),
}

/// Initialized touch input on whichever controller answered.
pub struct Gt911Tt21100Touch<I2C> {
    i2c: I2C,
    controller: Controller,
    config: Gt911Tt21100TouchConfig,
}

/// Static factory used by generated Board composition.
pub struct Gt911Tt21100TouchImplementation<I2C>(PhantomData<I2C>);

impl<I2C> PeripheralImplementation for Gt911Tt21100TouchImplementation<I2C>
where
    I2C: I2c + 'static,
{
    type Bindings = Gt911Tt21100TouchBindings<I2C>;
    type Config = Gt911Tt21100TouchConfig;
    type Peripheral = Gt911Tt21100Touch<I2C>;
    type Error = Gt911Tt21100TouchInitError;

    async fn initialize(
        mut bindings: Self::Bindings,
        config: Self::Config,
    ) -> Result<Self::Peripheral, Self::Error> {
        let tt21100 = Tt21100::new(config.tt21100_address).ok_or(
            Gt911Tt21100TouchInitError::InvalidAddress(config.tt21100_address),
        )?;
        let controller = match find_gt911(&mut bindings.i2c, config.gt911_address) {
            Ok(gt911) => Controller::Gt911(gt911),
            Err(Gt911TouchInitError::InvalidAddress(address)) => {
                return Err(Gt911Tt21100TouchInitError::InvalidAddress(address));
            }
            Err(Gt911TouchInitError::NotFound | Gt911TouchInitError::Bus(_)) => {
                if !tt21100.is_present(&mut bindings.i2c) {
                    return Err(Gt911Tt21100TouchInitError::NotFound);
                }
                Controller::Tt21100(tt21100)
            }
        };
        Ok(Gt911Tt21100Touch {
            i2c: bindings.i2c,
            controller,
            config,
        })
    }
}

impl<I2C> Gt911Tt21100Touch<I2C> {
    /// Returns the controller found at initialization.
    #[must_use]
    pub const fn controller_kind(&self) -> TouchControllerKind {
        match self.controller {
            Controller::Gt911(_) => TouchControllerKind::Gt911,
            Controller::Tt21100(_) => TouchControllerKind::Tt21100,
        }
    }
}

impl<I2C: I2c> Touch for Gt911Tt21100Touch<I2C> {
    type Error = Gt911Tt21100TouchError<I2C::Error>;

    fn descriptor(&self) -> TouchDescriptor {
        let max_points = match self.controller {
            Controller::Gt911(_) => GT911_MAX_POINTS,
            Controller::Tt21100(_) => TT21100_MAX_POINTS,
        };
        TouchDescriptor {
            width: self.config.width,
            height: self.config.height,
            max_points: u8::try_from(max_points).unwrap_or(u8::MAX),
        }
    }

    fn read_frame(&mut self) -> Result<TouchFrame, Self::Error> {
        match &self.controller {
            Controller::Gt911(chip) => read_gt911_frame(chip, &mut self.i2c),
            Controller::Tt21100(chip) => {
                let report = chip
                    .read_report(&mut self.i2c)
                    .map_err(Gt911TouchError::Bus)?;
                let mut frame = TouchFrame::new();
                for point in report.points() {
                    let x = if self.config.tt21100_mirror_x {
                        self.config.width.saturating_sub(1).saturating_sub(point.x)
                    } else {
                        point.x
                    };
                    frame.push(TouchPoint {
                        id: point.id,
                        x,
                        y: point.y,
                        strength: u16::from(point.pressure),
                    });
                }
                Ok(frame)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::arithmetic_side_effects)]

    use barracuda_peripheral::{PeripheralImplementation, touch::Touch};
    use embassy_futures::block_on;
    use embedded_hal::i2c::{ErrorKind, ErrorType, I2c, NoAcknowledgeSource, Operation};

    use super::{
        Gt911Tt21100TouchBindings, Gt911Tt21100TouchConfig, Gt911Tt21100TouchImplementation,
        Gt911Tt21100TouchInitError, TouchControllerKind,
    };

    /// One controller at `address`: a GT911 at 5Dh or 14h, a TT21100
    /// anywhere else.
    struct Bus {
        address: u8,
    }

    impl ErrorType for Bus {
        type Error = ErrorKind;
    }

    impl I2c for Bus {
        fn transaction(
            &mut self,
            address: u8,
            operations: &mut [Operation<'_>],
        ) -> Result<(), Self::Error> {
            if address != self.address {
                return Err(ErrorKind::NoAcknowledge(NoAcknowledgeSource::Address));
            }
            let gt911 = matches!(address, 0x5d | 0x14);
            let mut register = None;
            for operation in operations {
                match operation {
                    Operation::Write(bytes) => {
                        register = Some(u16::from_be_bytes([bytes[0], bytes[1]]));
                    }
                    Operation::Read(bytes) if gt911 => match register {
                        Some(0x8140) => bytes.copy_from_slice(b"911\0"),
                        _ => bytes.fill(0),
                    },
                    Operation::Read(bytes) => {
                        // One finger, track 2, at (10, 20).
                        let report = [
                            17, 0, 0x01, 0, 0, 0x01, 0, 0x00, 0x02, 10, 0, 20, 0, 9, 0, 0, 0,
                        ];
                        for (target, source) in bytes.iter_mut().zip(report) {
                            *target = source;
                        }
                    }
                }
            }
            Ok(())
        }
    }

    fn config() -> Gt911Tt21100TouchConfig {
        Gt911Tt21100TouchConfig::new(0x5d, 0x24, 320, 240, true)
    }

    #[test]
    fn prefers_a_gt911_at_either_address() {
        for address in [0x5d, 0x14] {
            let touch = block_on(Gt911Tt21100TouchImplementation::initialize(
                Gt911Tt21100TouchBindings::new(Bus { address }),
                config(),
            ))
            .expect("initialize");
            assert_eq!(touch.controller_kind(), TouchControllerKind::Gt911);
            assert_eq!(touch.descriptor().max_points, 5);
        }
    }

    #[test]
    fn falls_back_to_a_mirrored_tt21100() {
        let mut touch = block_on(Gt911Tt21100TouchImplementation::initialize(
            Gt911Tt21100TouchBindings::new(Bus { address: 0x24 }),
            config(),
        ))
        .expect("initialize");
        assert_eq!(touch.controller_kind(), TouchControllerKind::Tt21100);
        let frame = touch.read_frame().expect("frame");
        let point = frame.get(0).expect("one contact");
        assert_eq!(
            (point.id, point.x, point.y, point.strength),
            (2, 309, 20, 9)
        );
    }

    #[test]
    fn reports_no_controller() {
        let result = block_on(Gt911Tt21100TouchImplementation::initialize(
            Gt911Tt21100TouchBindings::new(Bus { address: 0x38 }),
            config(),
        ));
        assert!(matches!(result, Err(Gt911Tt21100TouchInitError::NotFound)));
    }
}
