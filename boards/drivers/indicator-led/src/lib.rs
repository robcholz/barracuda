//! Semantic driver for a Board-configured indicator LED.

#![no_std]

use embedded_hal::digital::{OutputPin, StatefulOutputPin};

/// Electrical level that turns an indicator on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ActiveLevel {
    /// The indicator is on while the pin is low.
    Low,
    /// The indicator is on while the pin is high.
    High,
}

/// Built-in indicator backed by one concrete `embedded-hal` output pin.
pub struct IndicatorLed<Pin> {
    pin: Pin,
    active: ActiveLevel,
}

impl<Pin> IndicatorLed<Pin> {
    /// Wraps the Board-owned output without changing its current latch.
    #[must_use]
    pub const fn new(pin: Pin, active: ActiveLevel) -> Self {
        Self { pin, active }
    }

    /// Returns the concrete output to its owner.
    #[must_use]
    pub fn into_inner(self) -> Pin {
        self.pin
    }
}

impl<Pin: OutputPin> IndicatorLed<Pin> {
    /// Turns the indicator on.
    ///
    /// # Errors
    ///
    /// Returns the concrete pin's output error.
    pub fn on(&mut self) -> Result<(), Pin::Error> {
        match self.active {
            ActiveLevel::Low => self.pin.set_low(),
            ActiveLevel::High => self.pin.set_high(),
        }
    }

    /// Turns the indicator off.
    ///
    /// # Errors
    ///
    /// Returns the concrete pin's output error.
    pub fn off(&mut self) -> Result<(), Pin::Error> {
        match self.active {
            ActiveLevel::Low => self.pin.set_high(),
            ActiveLevel::High => self.pin.set_low(),
        }
    }
}

impl<Pin: StatefulOutputPin> IndicatorLed<Pin> {
    /// Returns whether the output latch currently requests the on state.
    ///
    /// # Errors
    ///
    /// Returns the concrete pin's state-query error.
    pub fn is_on(&mut self) -> Result<bool, Pin::Error> {
        match self.active {
            ActiveLevel::Low => self.pin.is_set_low(),
            ActiveLevel::High => self.pin.is_set_high(),
        }
    }
}

#[cfg(test)]
mod tests {
    use core::convert::Infallible;
    use embedded_hal::digital::{ErrorType, OutputPin, StatefulOutputPin};

    use super::{ActiveLevel, IndicatorLed};

    struct Pin(bool);

    impl ErrorType for Pin {
        type Error = Infallible;
    }

    impl OutputPin for Pin {
        fn set_low(&mut self) -> Result<(), Self::Error> {
            self.0 = false;
            Ok(())
        }

        fn set_high(&mut self) -> Result<(), Self::Error> {
            self.0 = true;
            Ok(())
        }
    }

    impl StatefulOutputPin for Pin {
        fn is_set_high(&mut self) -> Result<bool, Self::Error> {
            Ok(self.0)
        }

        fn is_set_low(&mut self) -> Result<bool, Self::Error> {
            Ok(!self.0)
        }
    }

    #[test]
    fn active_level_is_a_board_driver_detail() {
        let mut high = IndicatorLed::new(Pin(false), ActiveLevel::High);
        high.on().expect("turn active-high indicator on");
        assert!(high.is_on().expect("read active-high indicator"));

        let mut low = IndicatorLed::new(Pin(true), ActiveLevel::Low);
        low.on().expect("turn active-low indicator on");
        assert!(low.is_on().expect("read active-low indicator"));
    }
}
