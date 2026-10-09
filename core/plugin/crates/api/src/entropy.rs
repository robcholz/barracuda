//! The Platform entropy source, shared with Plugins without its type.

use alloc::boxed::Box;
use core::fmt;

use barracuda_platform::{Entropy, EntropyUnavailable, UnavailableEntropy};
use portable_atomic_util::Arc;

/// The selected Platform's entropy source with its concrete type erased.
///
/// Plugins clone it to draw unpredictable bytes, for example to mint pairing
/// codes, without depending on the Platform. A Target without an entropy
/// source shares [`UnavailableEntropy`], which fails every request.
#[derive(Clone)]
pub struct SharedEntropy {
    source: Arc<dyn FillEntropy + Send + Sync>,
}

impl SharedEntropy {
    /// Shares `source`.
    #[must_use]
    pub fn new<E: Entropy + Send + Sync>(source: E) -> Self {
        Self {
            source: Arc::from(Box::new(source) as Box<dyn FillEntropy + Send + Sync>),
        }
    }

    /// An entropy source that fails every request.
    #[must_use]
    pub fn unavailable() -> Self {
        Self::new(UnavailableEntropy)
    }
}

impl Entropy for SharedEntropy {
    fn fill(&self, bytes: &mut [u8]) -> Result<(), EntropyUnavailable> {
        self.source.fill(bytes)
    }
}

impl fmt::Debug for SharedEntropy {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SharedEntropy")
            .finish_non_exhaustive()
    }
}

/// The object-safe part of [`Entropy`].
trait FillEntropy {
    fn fill(&self, bytes: &mut [u8]) -> Result<(), EntropyUnavailable>;
}

impl<E: Entropy> FillEntropy for E {
    fn fill(&self, bytes: &mut [u8]) -> Result<(), EntropyUnavailable> {
        Entropy::fill(self, bytes)
    }
}
