// Entropy for an ESP Platform built without its Wi-Fi radio, included by the
// Platform's `entropy` module.
//
// Without the radio, the chip's random number generator is true-random only
// while SAR ADC noise feeds it, and that noise source blocks the ADC. As
// Espressif recommends for an application that cannot keep a noise source
// running (ESP-IDF "Random Number Generation"), the entry runs the source only
// at start-up, before the Board takes the ADC, and seeds a ChaCha20 generator
// from it; every fill comes from that generator. After each fill the generator
// re-keys itself from its own output mixed with fresh hardware generator
// words, so the state it keeps reveals no earlier output.

use core::cell::RefCell;

use embassy_sync::blocking_mutex::{raw::CriticalSectionRawMutex, Mutex};
use rand_chacha::ChaCha20Rng;
use rand_core::{RngCore, SeedableRng};

/// Bytes of the ChaCha20 key and of the seed.
const KEY_BYTES: usize = 32;
/// Noise bytes folded into each seed byte.
const SEED_FOLD: usize = 4;

/// The generator `seed` starts; absent until then.
static GENERATOR: Mutex<CriticalSectionRawMutex, RefCell<Option<ChaCha20Rng>>> =
    Mutex::new(RefCell::new(None));

/// Seeds the generator from SAR ADC noise.
///
/// The noise source runs only while this reads it. Stopping it hands the SAR
/// ADC back, so the Board's ADC channels work afterwards. The entry calls it
/// once, before the Board takes the ADC and before any task runs.
pub(crate) fn seed(rng: esp_hal::peripherals::RNG<'_>, adc: esp_hal::peripherals::ADC1<'_>) {
    let source = esp_hal::rng::TrngSource::new(rng, adc);
    // The source is running, so the generator is true-random while it is read.
    let seed = esp_hal::rng::Trng::try_new().ok().map(|generator| {
        let mut noise = [0_u8; KEY_BYTES * SEED_FOLD];
        generator.read(&mut noise);
        let mut seed = [0_u8; KEY_BYTES];
        for chunk in noise.as_chunks::<KEY_BYTES>().0 {
            for (seed, noise) in seed.iter_mut().zip(chunk) {
                *seed ^= noise;
            }
        }
        seed
    });
    drop(source);
    if let Some(seed) = seed {
        GENERATOR.lock(|generator| {
            generator.replace(Some(ChaCha20Rng::from_seed(seed)));
        });
    }
}

/// Fills `bytes` from the generator, which [`seed`] must have started.
pub(crate) fn fill(bytes: &mut [u8]) -> Result<(), EntropyUnavailable> {
    GENERATOR.lock(|generator| {
        let mut generator = generator.borrow_mut();
        let current = generator.as_mut().ok_or(EntropyUnavailable)?;
        current.fill_bytes(bytes);
        let mut key = [0_u8; KEY_BYTES];
        current.fill_bytes(&mut key);
        let mut fresh = [0_u8; KEY_BYTES];
        esp_hal::rng::Rng::new().read(&mut fresh);
        for (key, fresh) in key.iter_mut().zip(fresh) {
            *key ^= fresh;
        }
        *current = ChaCha20Rng::from_seed(key);
        Ok(())
    })
}
