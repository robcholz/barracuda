//! The ESP32-C6 hardware random number generator as the Platform entropy source.

use barracuda_platform::{Entropy, EntropyUnavailable};

/// Entropy from the ESP32-C6 true random number generator.
///
/// The generator is only true-random while physical noise feeds it: the radio
/// running, or an ADC entropy source. Without one, [`Entropy::fill`] reports
/// [`EntropyUnavailable`] instead of returning pseudo-random bytes.
#[derive(Clone, Copy, Debug, Default)]
pub struct Esp32c6Entropy;

impl Entropy for Esp32c6Entropy {
    fn fill(&self, bytes: &mut [u8]) -> Result<(), EntropyUnavailable> {
        let generator = esp_hal::rng::Trng::try_new().map_err(|_error| EntropyUnavailable)?;
        generator.read(bytes);
        Ok(())
    }
}
