//! Reusable semantic driver for an indicator LED.

#![no_std]

use core::{convert::Infallible, marker::PhantomData};

use barracuda_peripheral::{
    PeripheralImplementation,
    indicator::{Indicator, IndicatorConfig},
};
use embedded_hal::digital::{OutputPin, StatefulOutputPin};

/// Electrical level that turns an indicator on.
pub use barracuda_peripheral::indicator::ActiveLevel;

/// Built-in indicator backed by one concrete `embedded-hal` output pin.
pub struct IndicatorLed<Pin> {
    pin: Pin,
    active: ActiveLevel,
}

/// Static implementation factory used by generated Board HAL composition.
pub struct IndicatorLedImplementation<Pin>(PhantomData<fn() -> Pin>);

impl<Pin: 'static> PeripheralImplementation for IndicatorLedImplementation<Pin> {
    type Bindings = Pin;
    type Config = IndicatorConfig;
    type Peripheral = IndicatorLed<Pin>;
    type Error = Infallible;

    async fn initialize(
        pin: Self::Bindings,
        config: Self::Config,
    ) -> Result<Self::Peripheral, Self::Error> {
        Ok(IndicatorLed::new(pin, config.active_level()))
    }
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

impl<Pin: StatefulOutputPin> Indicator for IndicatorLed<Pin> {
    type Error = Pin::Error;

    fn set_enabled(&mut self, enabled: bool) -> Result<(), Self::Error> {
        if enabled { self.on() } else { self.off() }
    }

    fn is_enabled(&mut self) -> Result<bool, Self::Error> {
        self.is_on()
    }
}

#[cfg(test)]
mod tests {
    use barracuda_peripheral::{PeripheralImplementation, indicator::Indicator};
    use core::convert::Infallible;
    use embedded_hal::digital::{ErrorType, OutputPin, StatefulOutputPin};

    use super::{ActiveLevel, IndicatorConfig, IndicatorLed, IndicatorLedImplementation};

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

    #[test]
    fn implements_the_stable_indicator_peripheral() {
        fn set_semantic_state<I: Indicator>(indicator: &mut I) -> Result<bool, I::Error> {
            indicator.set_enabled(true)?;
            indicator.is_enabled()
        }

        let mut indicator = IndicatorLed::new(Pin(false), ActiveLevel::High);
        assert!(set_semantic_state(&mut indicator).expect("stable indicator API"));
    }

    #[test]
    fn initializes_through_the_stable_driver_contract() {
        let _future = IndicatorLedImplementation::<Pin>::initialize(
            Pin(false),
            IndicatorConfig::new(ActiveLevel::High),
        );
    }
}
