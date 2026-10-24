//! The ESP32 hardware random number generator as the Platform entropy source.

use barracuda_platform::{Entropy, EntropyUnavailable};

/// Entropy from the ESP32 true random number generator.
///
/// The generator is only true-random while physical noise feeds it. With the
/// Wi-Fi radio built in, the running radio feeds it and each fill reads it.
/// Without the radio, SAR ADC noise feeds it at start-up only, so the ADC
/// stays free, and fills come from a ChaCha20 generator seeded from it (see
/// `seeded_entropy.rs`). Before a source feeds it, [`Entropy::fill`] reports
/// [`EntropyUnavailable`] instead of returning pseudo-random bytes.
#[derive(Clone, Copy, Debug, Default)]
pub struct Esp32Entropy;

impl Entropy for Esp32Entropy {
    fn fill(&self, bytes: &mut [u8]) -> Result<(), EntropyUnavailable> {
        fill(bytes)
    }
}

#[cfg(feature = "wifi")]
fn fill(bytes: &mut [u8]) -> Result<(), EntropyUnavailable> {
    let generator = esp_hal::rng::Trng::try_new().map_err(|_error| EntropyUnavailable)?;
    generator.read(bytes);
    Ok(())
}

#[cfg(not(feature = "wifi"))]
include!("../../esp32/src/seeded_entropy.rs");
