//! TLS client engine shared by every Platform.
//!
//! System builds one [`Tls`] from the Platform's entropy source. The trust
//! roots are Mozilla's CA store, compiled in, so every Platform trusts the same
//! roots. The HTTP client turns the handle into per-connection configuration.

#![no_std]

extern crate alloc;

#[cfg(target_os = "none")]
mod c_runtime;

#[cfg(not(target_os = "none"))]
use alloc::boxed::Box;
use alloc::string::String;

#[cfg(not(target_os = "none"))]
use alloc::string::ToString;
use barracuda_platform::{Entropy, EntropyUnavailable};

pub use mbedtls_rs::{Certificate, TlsReference, TlsVersion};

/// Mozilla's CA store as one NUL-terminated PEM bundle.
#[cfg(not(target_os = "none"))]
const TRUST_ROOTS: &str = concat!(include_str!("../roots/cacert.pem"), "\0");

/// The process-wide TLS engine and the roots it trusts.
///
/// mbedTLS allows one engine per process, so [`Tls::new`] succeeds once.
#[derive(Clone)]
pub struct Tls {
    #[cfg(not(target_os = "none"))]
    certificates: Certificate<'static>,
    #[cfg(not(target_os = "none"))]
    tls_reference: TlsReference<'static>,
    /// Device Platforms have no trust roots yet: the compiled-in PEM bundle
    /// is too large to parse into their RAM, and the compact bundle verified
    /// on demand is not built yet.
    #[cfg(target_os = "none")]
    never: core::convert::Infallible,
}

impl Tls {
    /// Initializes the TLS engine from the Platform's entropy source.
    ///
    /// # Errors
    ///
    /// Returns an error when the Platform has no entropy source, when this
    /// Platform has no trust roots, or when the engine is already initialized.
    pub fn new<E: Entropy + Send>(entropy: E) -> Result<Self, TlsError> {
        // mbedTLS cannot report a failed read, so refuse a source that fails now.
        entropy
            .fill(&mut [0; 1])
            .map_err(|EntropyUnavailable| TlsError::EntropyUnavailable)?;
        initialize(entropy)
    }

    /// The minimum protocol version for client connections.
    #[must_use]
    pub fn version(&self) -> TlsVersion {
        TlsVersion::Tls1_2
    }

    /// The roots that server certificate chains must lead to.
    #[must_use]
    pub fn certificates(&self) -> Certificate<'static> {
        #[cfg(not(target_os = "none"))]
        return self.certificates.clone();
        #[cfg(target_os = "none")]
        match self.never {}
    }

    /// The engine that client connections run on.
    #[must_use]
    pub fn reference(&self) -> TlsReference<'static> {
        #[cfg(not(target_os = "none"))]
        return self.tls_reference;
        #[cfg(target_os = "none")]
        match self.never {}
    }
}

#[cfg(not(target_os = "none"))]
fn initialize<E: Entropy + Send>(entropy: E) -> Result<Tls, TlsError> {
    use mbedtls_rs::X509;

    let pem = core::ffi::CStr::from_bytes_with_nul(TRUST_ROOTS.as_bytes())
        .map_err(|error| TlsError::InvalidTrustRoots(error.to_string()))?;
    let certificates = Certificate::new(X509::PEM(pem))
        .map_err(|error| TlsError::InvalidTrustRoots(error.to_string()))?;
    let rng = Box::leak(Box::new(rand_core::UnwrapErr(EntropyRng(entropy))));
    let tls = mbedtls_rs::Tls::new(rng)
        .map_err(|error| TlsError::Initialize(alloc::format!("{error:?}")))?;
    let tls = Box::leak(Box::new(tls));
    Ok(Tls {
        certificates,
        tls_reference: tls.reference(),
    })
}

#[cfg(target_os = "none")]
fn initialize<E: Entropy + Send>(_entropy: E) -> Result<Tls, TlsError> {
    Err(TlsError::NoTrustRoots)
}

/// Adapts a Platform entropy source to the generator mbedTLS draws from.
///
/// mbedTLS takes an infallible generator, so a source that fails after
/// [`Tls::new`] checked it panics rather than hand out weak bytes.
#[cfg(not(target_os = "none"))]
struct EntropyRng<E>(E);

#[cfg(not(target_os = "none"))]
impl<E: Entropy> rand_core::TryRng for EntropyRng<E> {
    type Error = EntropyUnavailable;

    fn try_next_u32(&mut self) -> Result<u32, Self::Error> {
        let mut bytes = [0; 4];
        self.0.fill(&mut bytes)?;
        Ok(u32::from_le_bytes(bytes))
    }

    fn try_next_u64(&mut self) -> Result<u64, Self::Error> {
        let mut bytes = [0; 8];
        self.0.fill(&mut bytes)?;
        Ok(u64::from_le_bytes(bytes))
    }

    fn try_fill_bytes(&mut self, bytes: &mut [u8]) -> Result<(), Self::Error> {
        self.0.fill(bytes)
    }
}

#[cfg(not(target_os = "none"))]
impl<E: Entropy> rand_core::TryCryptoRng for EntropyRng<E> {}

/// Failure while initializing the TLS engine.
#[derive(Debug, thiserror::Error)]
pub enum TlsError {
    /// The Platform has no entropy source.
    #[error("the Platform has no entropy source for TLS")]
    EntropyUnavailable,
    /// This Platform has no trust roots yet.
    #[error("no TLS trust roots on this Platform yet")]
    NoTrustRoots,
    /// The compiled-in trust roots could not be parsed.
    #[error("invalid TLS trust roots: {0}")]
    InvalidTrustRoots(String),
    /// The process-wide mbedTLS engine could not be initialized.
    #[error("failed to initialize mbedTLS: {0}")]
    Initialize(String),
}

#[cfg(test)]
mod tests {
    use barracuda_platform::{Entropy, EntropyUnavailable, UnavailableEntropy};

    use super::{Tls, TlsError};

    #[derive(Clone)]
    struct CountingEntropy;

    impl Entropy for CountingEntropy {
        fn fill(&self, bytes: &mut [u8]) -> Result<(), EntropyUnavailable> {
            for (index, byte) in bytes.iter_mut().enumerate() {
                *byte = index.to_le_bytes()[0] ^ 0x5a;
            }
            Ok(())
        }
    }

    #[test]
    fn missing_entropy_is_refused_before_the_engine_starts() {
        let result = Tls::new(UnavailableEntropy);
        assert!(matches!(result, Err(TlsError::EntropyUnavailable)));
    }

    #[test]
    fn engine_starts_once_from_platform_entropy_and_the_compiled_in_roots() {
        let first = Tls::new(CountingEntropy);
        assert!(first.is_ok(), "{:?}", first.err());
        let second = Tls::new(CountingEntropy);
        assert!(matches!(second, Err(TlsError::Initialize(_))));
    }
}
