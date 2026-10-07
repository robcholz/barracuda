//! The STM32U5 true random number generator as the Platform entropy source.

use core::cell::RefCell;

use barracuda_platform::{Entropy, EntropyUnavailable};
use embassy_stm32::{peripherals::RNG, rng::Rng};
use embassy_sync::blocking_mutex::{raw::CriticalSectionRawMutex, Mutex};

/// The RNG peripheral driver, shared by every [`Stm32Entropy`].
pub type Stm32Rng = Rng<'static, RNG>;

/// Entropy from the STM32U5 true random number generator, which samples
/// analog noise sources. The driver recovers from seed and clock errors by
/// resetting the generator, as the reference manual prescribes.
#[derive(Clone, Copy)]
pub struct Stm32Entropy {
    rng: &'static Mutex<CriticalSectionRawMutex, RefCell<Stm32Rng>>,
}

impl Stm32Entropy {
    /// Shares the initialized RNG driver.
    #[must_use]
    pub fn new(rng: &'static Mutex<CriticalSectionRawMutex, RefCell<Stm32Rng>>) -> Self {
        Self { rng }
    }
}

impl Entropy for Stm32Entropy {
    fn fill(&self, bytes: &mut [u8]) -> Result<(), EntropyUnavailable> {
        self.rng.lock(|rng| rng.borrow_mut().fill_bytes(bytes));
        Ok(())
    }
}
