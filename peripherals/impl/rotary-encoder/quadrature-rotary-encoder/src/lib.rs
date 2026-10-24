//! Incremental rotary encoder decoded by a Platform pulse counter.
//!
//! The Platform's `pulse-counter` binding counts every edge of the encoder's
//! A and B signals in hardware, up while B leads A. This implementation keeps
//! the origin of [`RotaryEncoder::position`] and the mark of
//! [`RotaryEncoder::delta`], and flips the sign when the Board says so.

#![no_std]

use core::marker::PhantomData;

use barracuda_board_hal::QuadratureDecoder;
use barracuda_peripheral::{PeripheralImplementation, rotary_encoder::RotaryEncoder};

/// Board-owned direction of the count.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct QuadratureRotaryEncoderConfig {
    reverse: bool,
}

impl QuadratureRotaryEncoderConfig {
    /// Counts up while A leads B when `reverse`, while B leads A otherwise.
    #[must_use]
    pub const fn new(reverse: bool) -> Self {
        Self { reverse }
    }
}

/// Initialized rotary encoder.
pub struct QuadratureRotaryEncoder<C> {
    counter: C,
    reverse: bool,
    origin: i64,
    mark: i64,
}

/// Static factory used by generated Board composition.
pub struct QuadratureRotaryEncoderImplementation<C>(PhantomData<fn() -> C>);

impl<C> PeripheralImplementation for QuadratureRotaryEncoderImplementation<C>
where
    C: QuadratureDecoder + 'static,
{
    type Bindings = C;
    type Config = QuadratureRotaryEncoderConfig;
    type Peripheral = QuadratureRotaryEncoder<C>;
    type Error = C::Error;

    async fn initialize(
        mut counter: Self::Bindings,
        config: Self::Config,
    ) -> Result<Self::Peripheral, Self::Error> {
        let origin = counter.count()?;
        Ok(QuadratureRotaryEncoder {
            counter,
            reverse: config.reverse,
            origin,
            mark: 0,
        })
    }
}

impl<C: QuadratureDecoder> RotaryEncoder for QuadratureRotaryEncoder<C> {
    type Error = C::Error;

    fn position(&mut self) -> Result<i64, Self::Error> {
        let edges = self.counter.count()?.saturating_sub(self.origin);
        Ok(if self.reverse {
            edges.saturating_neg()
        } else {
            edges
        })
    }

    fn delta(&mut self) -> Result<i64, Self::Error> {
        let position = self.position()?;
        let delta = position.saturating_sub(self.mark);
        self.mark = position;
        Ok(delta)
    }

    fn reset(&mut self) -> Result<(), Self::Error> {
        self.origin = self.counter.count()?;
        self.mark = 0;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    extern crate std;

    use core::convert::Infallible;
    use std::cell::Cell;
    use std::rc::Rc;

    use barracuda_board_hal::QuadratureDecoder;
    use barracuda_peripheral::{PeripheralImplementation, rotary_encoder::RotaryEncoder};
    use embassy_futures::block_on;

    use super::{QuadratureRotaryEncoderConfig, QuadratureRotaryEncoderImplementation};

    struct Counter(Rc<Cell<i64>>);

    impl QuadratureDecoder for Counter {
        type Error = Infallible;

        fn count(&mut self) -> Result<i64, Self::Error> {
            Ok(self.0.get())
        }
    }

    #[test]
    fn counts_from_start_and_reports_deltas() {
        let edges = Rc::new(Cell::new(40));
        let mut encoder = block_on(QuadratureRotaryEncoderImplementation::initialize(
            Counter(Rc::clone(&edges)),
            QuadratureRotaryEncoderConfig::new(false),
        ))
        .expect("initialize");
        assert_eq!(encoder.position(), Ok(0));
        edges.set(48);
        assert_eq!(encoder.delta(), Ok(8));
        edges.set(44);
        assert_eq!(encoder.delta(), Ok(-4));
        assert_eq!(encoder.position(), Ok(4));
        encoder.reset().expect("reset");
        assert_eq!(encoder.position(), Ok(0));
        edges.set(45);
        assert_eq!(encoder.delta(), Ok(1));
    }

    #[test]
    fn reverses_the_count() {
        let edges = Rc::new(Cell::new(0));
        let mut encoder = block_on(QuadratureRotaryEncoderImplementation::initialize(
            Counter(Rc::clone(&edges)),
            QuadratureRotaryEncoderConfig::new(true),
        ))
        .expect("initialize");
        edges.set(12);
        assert_eq!(encoder.position(), Ok(-12));
        assert_eq!(encoder.delta(), Ok(-12));
    }
}
