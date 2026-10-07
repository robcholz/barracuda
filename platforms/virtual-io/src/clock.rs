//! Monotonic time base shared by every virtual resource of one hardware model.

use std::{
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};

/// Monotonic clock that timestamps bus transactions and pin changes.
///
/// The host Platform uses [`Clock::real`], so recorded times follow the
/// process clock that Embassy timers also use. Driver tests use
/// [`Clock::manual`], which only advances through [`Clock::advance`] or a
/// [`VirtualDelay`], so datasheet timing rules are checked deterministically
/// and without sleeping.
#[derive(Clone, Debug)]
pub struct Clock {
    source: Source,
}

#[derive(Clone, Debug)]
enum Source {
    Real(Instant),
    Manual(Arc<AtomicU64>),
}

impl Clock {
    /// Creates a clock that follows the process monotonic clock from now.
    #[must_use]
    pub fn real() -> Self {
        Self {
            source: Source::Real(Instant::now()),
        }
    }

    /// Creates a clock that starts at zero and moves only when advanced.
    #[must_use]
    pub fn manual() -> Self {
        Self {
            source: Source::Manual(Arc::new(AtomicU64::new(0))),
        }
    }

    /// Returns the time elapsed since the clock was created.
    #[must_use]
    pub fn now(&self) -> Duration {
        match &self.source {
            Source::Real(epoch) => epoch.elapsed(),
            Source::Manual(nanos) => Duration::from_nanos(nanos.load(Ordering::Acquire)),
        }
    }

    /// Moves a manual clock forward; a real clock sleeps for the duration.
    pub fn advance(&self, duration: Duration) {
        match &self.source {
            Source::Real(_) => std::thread::sleep(duration),
            Source::Manual(nanos) => {
                let step = u64::try_from(duration.as_nanos()).unwrap_or(u64::MAX);
                let _previous = nanos.fetch_add(step, Ordering::AcqRel);
            }
        }
    }
}

/// Delay that advances the virtual clock, for drivers under test.
///
/// It implements the blocking and async `embedded-hal` delay contracts. On a
/// manual clock a delay returns immediately after moving time forward.
#[derive(Clone, Debug)]
pub struct VirtualDelay {
    clock: Clock,
}

impl VirtualDelay {
    /// Creates a delay over `clock`.
    #[must_use]
    pub const fn new(clock: Clock) -> Self {
        Self { clock }
    }
}

impl embedded_hal::delay::DelayNs for VirtualDelay {
    fn delay_ns(&mut self, ns: u32) {
        self.clock.advance(Duration::from_nanos(u64::from(ns)));
    }
}

impl embedded_hal_async::delay::DelayNs for VirtualDelay {
    async fn delay_ns(&mut self, ns: u32) {
        self.clock.advance(Duration::from_nanos(u64::from(ns)));
    }
}

#[cfg(test)]
mod tests {
    use embedded_hal::delay::DelayNs as _;

    use super::*;

    #[test]
    fn a_manual_clock_moves_only_through_delays() {
        let clock = Clock::manual();
        assert_eq!(clock.now(), Duration::ZERO);
        let mut delay = VirtualDelay::new(clock.clone());
        delay.delay_ms(5);
        delay.delay_us(250);
        assert_eq!(clock.now(), Duration::from_micros(5_250));
    }
}
