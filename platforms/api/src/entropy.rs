//! Platform-owned randomness.

/// Unpredictable bytes from the Platform's entropy source.
///
/// A source is suitable for cryptographic use: the operating system's
/// generator on Host Platforms, a hardware true random number generator fed by
/// physical noise on device Platforms. A Platform without such a source
/// supplies [`UnavailableEntropy`] rather than a weaker generator.
pub trait Entropy: Clone + 'static {
    /// Fills `bytes` with unpredictable bytes.
    ///
    /// # Errors
    ///
    /// Returns [`EntropyUnavailable`] when the Platform has no entropy source
    /// running, leaving `bytes` unspecified.
    fn fill(&self, bytes: &mut [u8]) -> Result<(), EntropyUnavailable>;
}

/// Explicitly unsupported entropy for Platforms without an entropy source.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct UnavailableEntropy;

impl Entropy for UnavailableEntropy {
    fn fill(&self, _bytes: &mut [u8]) -> Result<(), EntropyUnavailable> {
        Err(EntropyUnavailable)
    }
}

/// The Platform has no entropy source running.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EntropyUnavailable;

impl core::fmt::Display for EntropyUnavailable {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter.write_str("the Platform has no entropy source")
    }
}

impl core::error::Error for EntropyUnavailable {}
