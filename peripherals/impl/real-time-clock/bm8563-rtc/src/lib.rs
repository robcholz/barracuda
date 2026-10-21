//! BM8563 and PCF8563 real-time clock implementation.

#![no_std]

use core::marker::PhantomData;

use barracuda_driver_bm8563::{Bm8563, ClockData, Error as ChipError};
use barracuda_peripheral::{PeripheralImplementation, real_time_clock::RealTimeClock};
use embedded_hal::i2c::I2c;

pub use barracuda_peripheral::real_time_clock::DateTime;

/// Board-owned RTC configuration.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Bm8563Config {
    address: u8,
}

impl Bm8563Config {
    /// Creates an RTC configuration for one seven-bit I2C address.
    #[must_use]
    pub const fn new(address: u8) -> Self {
        Self { address }
    }
}

/// Move-only resources consumed by the RTC implementation.
pub struct Bm8563Bindings<I2C> {
    i2c: I2C,
}

impl<I2C> Bm8563Bindings<I2C> {
    /// Wraps an I2C device view.
    #[must_use]
    pub const fn new(i2c: I2C) -> Self {
        Self { i2c }
    }
}

/// RTC initialization failure.
#[derive(Debug)]
pub enum Bm8563InitError<BusError> {
    /// The configured address is not a seven-bit I2C address.
    InvalidAddress(u8),
    /// The clock did not accept its start-up writes.
    Bus(BusError),
}

/// RTC access failure.
#[derive(Debug)]
pub enum Bm8563Error<BusError> {
    /// A register operation failed.
    Bus(BusError),
    /// The clock registers did not hold a valid date and time.
    InvalidClockData,
    /// The requested date and time is outside 1900 through 2099.
    InvalidDateTime,
}

/// Initialized BM8563 clock.
pub struct Bm8563Rtc<I2C> {
    i2c: I2C,
    chip: Bm8563,
}

/// Static factory used by generated Board composition.
pub struct Bm8563RtcImplementation<I2C>(PhantomData<I2C>);

impl<I2C> PeripheralImplementation for Bm8563RtcImplementation<I2C>
where
    I2C: I2c + 'static,
{
    type Bindings = Bm8563Bindings<I2C>;
    type Config = Bm8563Config;
    type Peripheral = Bm8563Rtc<I2C>;
    type Error = Bm8563InitError<I2C::Error>;

    async fn initialize(
        mut bindings: Self::Bindings,
        config: Self::Config,
    ) -> Result<Self::Peripheral, Self::Error> {
        let chip =
            Bm8563::new(config.address).ok_or(Bm8563InitError::InvalidAddress(config.address))?;
        chip.start(&mut bindings.i2c)
            .map_err(Bm8563InitError::Bus)?;
        Ok(Bm8563Rtc {
            i2c: bindings.i2c,
            chip,
        })
    }
}

impl<I2C> RealTimeClock for Bm8563Rtc<I2C>
where
    I2C: I2c,
{
    type Error = Bm8563Error<I2C::Error>;

    /// Reports the voltage-low flag.
    fn lost_power(&mut self) -> Result<bool, Self::Error> {
        self.chip
            .lost_power(&mut self.i2c)
            .map_err(Bm8563Error::Bus)
    }

    /// Reads one coherent set of clock and calendar registers.
    fn read_datetime(&mut self) -> Result<DateTime, Self::Error> {
        let clock = self
            .chip
            .read_clock(&mut self.i2c)
            .map_err(map_chip_error)?;
        Ok(DateTime {
            year: clock.year,
            month: clock.month,
            day: clock.day,
            weekday: clock.weekday,
            hour: clock.hour,
            minute: clock.minute,
            second: clock.second,
        })
    }

    /// Replaces the retained time, which clears the voltage-low flag.
    fn set_datetime(&mut self, datetime: DateTime) -> Result<(), Self::Error> {
        self.chip
            .write_clock(
                &mut self.i2c,
                ClockData {
                    year: datetime.year,
                    month: datetime.month,
                    day: datetime.day,
                    weekday: datetime.weekday,
                    hour: datetime.hour,
                    minute: datetime.minute,
                    second: datetime.second,
                },
            )
            .map_err(map_chip_error)
    }
}

fn map_chip_error<E>(error: ChipError<E>) -> Bm8563Error<E> {
    match error {
        ChipError::Bus(error) => Bm8563Error::Bus(error),
        ChipError::InvalidClockData => Bm8563Error::InvalidClockData,
        ChipError::InvalidDateTime => Bm8563Error::InvalidDateTime,
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::arithmetic_side_effects)]

    use core::convert::Infallible;

    use barracuda_peripheral::{
        PeripheralImplementation,
        real_time_clock::{DateTime, RealTimeClock},
    };
    use embassy_futures::block_on;
    use embedded_hal::i2c::{ErrorType, I2c, Operation};

    use super::{Bm8563Bindings, Bm8563Config, Bm8563Error, Bm8563RtcImplementation};

    struct Clock {
        registers: [u8; 0x10],
    }

    impl ErrorType for Clock {
        type Error = Infallible;
    }

    impl I2c for Clock {
        fn transaction(
            &mut self,
            _address: u8,
            operations: &mut [Operation<'_>],
        ) -> Result<(), Self::Error> {
            let mut pointer = 0;
            for operation in operations {
                match operation {
                    Operation::Write(bytes) => {
                        pointer = usize::from(bytes[0]);
                        for (offset, value) in bytes[1..].iter().enumerate() {
                            self.registers[pointer + offset] = *value;
                        }
                    }
                    Operation::Read(bytes) => {
                        for (offset, value) in bytes.iter_mut().enumerate() {
                            *value = self.registers[pointer + offset];
                        }
                    }
                }
            }
            Ok(())
        }
    }

    #[test]
    fn keeps_time_across_a_set_and_clears_the_voltage_low_flag() {
        let mut registers = [0; 0x10];
        // Stopped clock with VL set, as after a supply loss.
        registers[0x00] = 0x20;
        registers[0x02] = 0x80;
        let mut rtc = block_on(Bm8563RtcImplementation::initialize(
            Bm8563Bindings::new(Clock { registers }),
            Bm8563Config::new(0x51),
        ))
        .expect("initialize");
        assert_eq!(rtc.i2c.registers[0x00], 0x00, "clock running");
        assert!(rtc.lost_power().expect("flag"));
        assert!(matches!(
            rtc.read_datetime(),
            Err(Bm8563Error::InvalidClockData)
        ));

        let now = DateTime {
            year: 2026,
            month: 10,
            day: 21,
            weekday: 3,
            hour: 9,
            minute: 30,
            second: 0,
        };
        rtc.set_datetime(now).expect("set");
        assert!(!rtc.lost_power().expect("flag"));
        assert_eq!(rtc.read_datetime().expect("read"), now);
    }

    #[test]
    fn rejects_a_non_seven_bit_address() {
        let result = block_on(Bm8563RtcImplementation::initialize(
            Bm8563Bindings::new(Clock {
                registers: [0; 0x10],
            }),
            Bm8563Config::new(0xa2),
        ));
        assert!(result.is_err());
    }
}
