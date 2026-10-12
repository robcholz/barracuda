//! The operating system's random number generator as the Platform entropy
//! source.

use barracuda_platform::{Entropy, EntropyUnavailable};

/// Entropy from the operating system, through `getrandom`.
#[derive(Clone, Copy, Debug, Default)]
pub struct LinuxEntropy;

impl Entropy for LinuxEntropy {
    fn fill(&self, bytes: &mut [u8]) -> Result<(), EntropyUnavailable> {
        getrandom::fill(bytes).map_err(|_error| EntropyUnavailable)
    }
}
