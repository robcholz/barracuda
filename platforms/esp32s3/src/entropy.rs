//! The ESP32-S3 hardware random number generator as the Platform entropy source.

use barracuda_platform::{Entropy, EntropyUnavailable};

/// Entropy from the ESP32-S3 true random number generator.
///
/// The generator is only true-random while physical noise feeds it: the radio
/// running, or an ADC entropy source. Without one, [`Entropy::fill`] reports
/// [`EntropyUnavailable`] instead of returning pseudo-random bytes.
#[derive(Clone, Copy, Debug, Default)]
pub struct Esp32S3Entropy;

impl Entropy for Esp32S3Entropy {
    fn fill(&self, bytes: &mut [u8]) -> Result<(), EntropyUnavailable> {
        let generator = esp_hal::rng::Trng::try_new().map_err(|_error| EntropyUnavailable)?;
        generator.read(bytes);
        Ok(())
    }
}
