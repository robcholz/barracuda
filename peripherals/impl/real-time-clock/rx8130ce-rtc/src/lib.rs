//! RX8130CE real-time clock implementation.

#![no_std]

use core::marker::PhantomData;

use barracuda_driver_rx8130ce::{ClockData, Error as ChipError, Rx8130ce};
use barracuda_peripheral::{PeripheralImplementation, real_time_clock::RealTimeClock};
use embedded_hal::i2c::I2c;

pub use barracuda_peripheral::real_time_clock::DateTime;

/// Board-owned RTC configuration.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rx8130ceConfig {
    address: u8,
}

impl Rx8130ceConfig {
    /// Creates an RTC configuration for one seven-bit I2C address.
    #[must_use]
    pub const fn new(address: u8) -> Self {
        Self { address }
    }
}

/// Move-only resources consumed by the RTC implementation.
pub struct Rx8130ceBindings<I2C> {
    i2c: I2C,
}

impl<I2C> Rx8130ceBindings<I2C> {
    /// Wraps an I2C device view.
    #[must_use]
    pub const fn new(i2c: I2C) -> Self {
        Self { i2c }
    }
}

/// RTC initialization failure.
#[derive(Debug)]
pub enum Rx8130ceInitError<BusError> {
    /// The configured address is not a seven-bit I2C address.
    InvalidAddress(u8),
    /// The RTC did not answer the probe read.
    Bus(BusError),
}

/// RTC access failure.
#[derive(Debug)]
pub enum Rx8130ceError<BusError> {
    /// A register operation failed.
    Bus(BusError),
    /// Clock registers did not contain a valid date and time.
    InvalidClockData,
    /// The requested date and time is outside the supported range.
    InvalidDateTime,
}

/// Initialized RX8130CE clock.
pub struct Rx8130ceRtc<I2C> {
    i2c: I2C,
    chip: Rx8130ce,
}

/// Static factory used by generated Board composition.
pub struct Rx8130ceRtcImplementation<I2C>(PhantomData<I2C>);

impl<I2C> PeripheralImplementation for Rx8130ceRtcImplementation<I2C>
where
    I2C: I2c + 'static,
{
    type Bindings = Rx8130ceBindings<I2C>;
    type Config = Rx8130ceConfig;
    type Peripheral = Rx8130ceRtc<I2C>;
    type Error = Rx8130ceInitError<I2C::Error>;

    async fn initialize(
        mut bindings: Self::Bindings,
        config: Self::Config,
    ) -> Result<Self::Peripheral, Self::Error> {
        let chip = Rx8130ce::new(config.address)
            .ok_or(Rx8130ceInitError::InvalidAddress(config.address))?;
        chip.probe(&mut bindings.i2c)
            .map_err(Rx8130ceInitError::Bus)?;
        Ok(Rx8130ceRtc {
            i2c: bindings.i2c,
            chip,
        })
    }
}

impl<I2C> RealTimeClock for Rx8130ceRtc<I2C>
where
    I2C: I2c,
{
    type Error = Rx8130ceError<I2C::Error>;

    /// Reports whether supply loss may have invalidated the retained time.
    fn lost_power(&mut self) -> Result<bool, Self::Error> {
        self.chip
            .lost_power(&mut self.i2c)
            .map_err(Rx8130ceError::Bus)
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

    /// Replaces the retained time and clears the voltage-loss flag.
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

fn map_chip_error<E>(error: ChipError<E>) -> Rx8130ceError<E> {
    match error {
        ChipError::Bus(error) => Rx8130ceError::Bus(error),
        ChipError::InvalidClockData => Rx8130ceError::InvalidClockData,
        ChipError::InvalidDateTime => Rx8130ceError::InvalidDateTime,
    }
}
