//! The M5Stack PowerHub's real-time clock, relayed by its power co-processor.
//!
//! The RX8130CE is on the co-processor's own bus; the co-processor relays
//! its time and date in binary at registers C0h-C6h (M5Unified
//! `RTC_PowerHub_Class`). The co-processor exposes no confirmed
//! supply-loss flag (M5Unified's `getVoltLow` reads register 02h, which is
//! Port A's power switch), so the clock reports lost power when its
//! registers hold no valid date and time, which is what an RX8130CE that
//! lost its backup reads as.

#![no_std]

use core::marker::PhantomData;

use barracuda_driver_m5powerhub::{ClockData, Error as ChipError, M5PowerHub};
use barracuda_peripheral::{
    PeripheralImplementation,
    real_time_clock::{DateTime, RealTimeClock},
};
use embedded_hal::i2c::I2c;

/// Board-owned co-processor address.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PowerHubRtcConfig {
    address: u8,
}

impl PowerHubRtcConfig {
    /// Creates configuration for the co-processor at `address`.
    #[must_use]
    pub const fn new(address: u8) -> Self {
        Self { address }
    }
}

/// Clock failure.
#[derive(Debug, PartialEq, Eq)]
pub enum PowerHubRtcError<E> {
    /// The address is not a seven-bit address.
    InvalidAddress(u8),
    /// The co-processor could not be read or written.
    Bus(E),
    /// The clock registers hold no valid date and time.
    InvalidClockData,
    /// The date or time cannot be stored.
    InvalidDateTime,
}

impl<E> From<ChipError<E>> for PowerHubRtcError<E> {
    fn from(error: ChipError<E>) -> Self {
        match error {
            ChipError::Bus(error) => Self::Bus(error),
            ChipError::InvalidClockData => Self::InvalidClockData,
            ChipError::InvalidDateTime => Self::InvalidDateTime,
        }
    }
}

/// Initialized clock.
pub struct PowerHubRtc<I2C> {
    i2c: I2C,
    hub: M5PowerHub,
}

/// Static factory used by generated Board composition.
pub struct PowerHubRtcImplementation<I2C>(PhantomData<fn() -> I2C>);

impl<I2C> PeripheralImplementation for PowerHubRtcImplementation<I2C>
where
    I2C: I2c + 'static,
{
    type Bindings = I2C;
    type Config = PowerHubRtcConfig;
    type Peripheral = PowerHubRtc<I2C>;
    type Error = PowerHubRtcError<I2C::Error>;

    async fn initialize(i2c: I2C, config: Self::Config) -> Result<Self::Peripheral, Self::Error> {
        let hub = M5PowerHub::new(config.address)
            .ok_or(PowerHubRtcError::InvalidAddress(config.address))?;
        let mut clock = PowerHubRtc { i2c, hub };
        // Probe the co-processor; an unset clock is not a start-up failure.
        match clock.hub.read_clock(&mut clock.i2c) {
            Ok(_) | Err(ChipError::InvalidClockData) => Ok(clock),
            Err(error) => Err(error.into()),
        }
    }
}

impl<I2C: I2c> RealTimeClock for PowerHubRtc<I2C> {
    type Error = PowerHubRtcError<I2C::Error>;

    fn lost_power(&mut self) -> Result<bool, Self::Error> {
        match self.hub.read_clock(&mut self.i2c) {
            Ok(_) => Ok(false),
            Err(ChipError::InvalidClockData) => Ok(true),
            Err(error) => Err(error.into()),
        }
    }

    fn read_datetime(&mut self) -> Result<DateTime, Self::Error> {
        let clock = self.hub.read_clock(&mut self.i2c)?;
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

    fn set_datetime(&mut self, datetime: DateTime) -> Result<(), Self::Error> {
        self.hub
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
            .map_err(Into::into)
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::arithmetic_side_effects)]
mod tests {
    extern crate std;

    use core::convert::Infallible;

    use barracuda_peripheral::{
        PeripheralImplementation,
        real_time_clock::{DateTime, RealTimeClock},
    };
    use embassy_futures::block_on;
    use embedded_hal::i2c::{ErrorType, I2c, Operation};

    use super::{PowerHubRtcConfig, PowerHubRtcImplementation};

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
            let mut register = 0;
            for operation in operations {
                match operation {
                    Operation::Write(bytes) => {
                        register = usize::from(bytes[0]);
                        for (offset, value) in bytes[1..].iter().enumerate() {
                            self.0[register + offset] = *value;
                        }
                    }
                    Operation::Read(values) => {
                        for (offset, value) in values.iter_mut().enumerate() {
                            *value = self.0[register + offset];
                        }
                    }
                }
            }
            Ok(())
        }
    }

    #[test]
    fn an_unset_clock_starts_and_reports_lost_power_until_set() {
        let mut clock = block_on(PowerHubRtcImplementation::initialize(
            Registers([0; 256]),
            PowerHubRtcConfig::new(0x50),
        ))
        .expect("start");
        assert_eq!(clock.lost_power(), Ok(true));
        let datetime = DateTime {
            year: 2026,
            month: 10,
            day: 22,
            weekday: 4,
            hour: 12,
            minute: 34,
            second: 56,
        };
        clock.set_datetime(datetime).expect("set");
        assert_eq!(clock.i2c.0[0xc0..0xc7], [56, 34, 12, 22, 10, 26, 10]);
        assert_eq!(clock.lost_power(), Ok(false));
        assert_eq!(clock.read_datetime(), Ok(datetime));
    }
}
