//! Installable wall clock used to stamp human-facing Session metadata.

use alloc::rc::Rc;
use core::cell::RefCell;

/// Reads the current wall-clock time in Unix milliseconds.
///
/// Returns `None` while the clock is not synchronized.
pub type WallClock = Rc<dyn Fn() -> Option<u64>>;

/// Shared slot for the runtime's [`WallClock`].
///
/// Every clone observes the same slot, so a clock installed after Sessions
/// already run reaches their actors too. Without an installed clock, `now`
/// returns `None`.
#[derive(Clone, Default)]
pub struct SessionClock(Rc<RefCell<Option<WallClock>>>);

impl SessionClock {
    /// Installs or replaces the wall clock seen by every clone.
    pub fn install(&self, clock: WallClock) {
        *self.0.borrow_mut() = Some(clock);
    }

    /// Reads the installed clock, or `None` when none is installed or it is
    /// not synchronized.
    #[must_use]
    pub fn now(&self) -> Option<u64> {
        let clock = self.0.borrow().clone()?;
        clock()
    }
}

#[cfg(test)]
mod tests {
    use alloc::rc::Rc;

    use super::SessionClock;

    #[test]
    fn installation_reaches_existing_clones() {
        let clock = SessionClock::default();
        let shared = clock.clone();
        assert_eq!(shared.now(), None);

        clock.install(Rc::new(|| Some(42)));
        assert_eq!(shared.now(), Some(42));

        clock.install(Rc::new(|| None));
        assert_eq!(shared.now(), None);
    }
}
