//! Platform-owned TLS client capability.
//!
//! Concrete Platforms initialize trust roots, randomness, and the TLS engine.
//! HTTP consumers only request a fresh client configuration.

#![no_std]

extern crate alloc;

#[cfg(feature = "mbedtls")]
use alloc::{boxed::Box, string::String, string::ToString};

#[cfg(any(feature = "embedded-tls", feature = "mbedtls"))]
use http_client::TlsConfig;
#[cfg(feature = "mbedtls")]
use mbedtls_rs::{Certificate, Tls, TlsReference, TlsVersion, X509};

/// TLS capability consumed by HTTP-client owners.
pub trait ClientTls: Clone + 'static {
    /// Creates an independent TLS configuration for one HTTP client.
    fn config(&self) -> Option<TlsConfig<'static>>;
}

/// Explicit plaintext capability for tests and deliberately HTTP-only systems.
#[derive(Clone, Copy, Debug, Default)]
pub struct PlaintextTls;

impl ClientTls for PlaintextTls {
    fn config(&self) -> Option<TlsConfig<'static>> {
        None
    }
}

/// Reusable mbedTLS capability backed by Platform-selected trust roots.
#[derive(Clone)]
#[cfg(feature = "mbedtls")]
pub struct MbedTls {
    certificates: Certificate<'static>,
    tls_reference: TlsReference<'static>,
}

/// Raw embedded inputs from which a Platform initializes its TLS capability.
#[cfg(feature = "mbedtls")]
pub struct MbedTlsInput {
    rng: &'static mut (dyn rand_core::CryptoRng + Send),
    trust_roots: &'static [u8],
}

#[cfg(feature = "mbedtls")]
impl MbedTlsInput {
    /// Creates embedded mbedTLS inputs from a Platform RNG and DER trust roots.
    #[must_use]
    pub fn new(
        rng: &'static mut (dyn rand_core::CryptoRng + Send),
        trust_roots: &'static [u8],
    ) -> Self {
        Self { rng, trust_roots }
    }

    /// Consumes the raw inputs and initializes the Platform TLS capability.
    ///
    /// # Errors
    ///
    /// Returns an error when the roots or global mbedTLS engine are invalid.
    pub fn initialize(self) -> Result<MbedTls, TlsError> {
        MbedTls::from_der(self.rng, self.trust_roots)
    }
}

#[cfg(feature = "mbedtls")]
impl MbedTls {
    /// Initializes mbedTLS from DER-encoded trust roots and a Platform RNG.
    ///
    /// # Errors
    ///
    /// Returns an error when the trust roots cannot be parsed or the process-wide
    /// mbedTLS engine has already been initialized.
    pub fn from_der(
        rng: &'static mut (dyn rand_core::CryptoRng + Send),
        certificate: &[u8],
    ) -> Result<Self, TlsError> {
        Self::from_x509(rng, X509::DER(certificate))
    }

    /// Initializes mbedTLS from a NUL-terminated PEM trust-root bundle.
    ///
    /// Platforms own discovery and reading of the bundle; this method only
    /// initializes the shared TLS engine from the supplied bytes.
    ///
    /// # Errors
    ///
    /// Returns an error when the trust roots or global mbedTLS engine are
    /// invalid.
    #[cfg(feature = "pem")]
    pub fn from_pem(
        rng: &'static mut (dyn rand_core::CryptoRng + Send),
        certificate: &core::ffi::CStr,
    ) -> Result<Self, TlsError> {
        Self::from_x509(rng, X509::PEM(certificate))
    }

    /// Initializes mbedTLS from one owned/copying X.509 input.
    fn from_x509(
        rng: &'static mut (dyn rand_core::CryptoRng + Send),
        certificate: X509<'_>,
    ) -> Result<Self, TlsError> {
        let certificates = Certificate::new(certificate)
            .map_err(|error| TlsError::InvalidTrustRoots(error.to_string()))?;
        let tls =
            Tls::new(rng).map_err(|error| TlsError::Initialize(alloc::format!("{error:?}")))?;
        let tls = Box::leak(Box::new(tls));
        Ok(Self {
            certificates,
            tls_reference: tls.reference(),
        })
    }

    /// Creates one verified shared HTTP-client TLS configuration.
    #[must_use]
    pub fn client_config(&self) -> TlsConfig<'static> {
        TlsConfig::new(
            TlsVersion::Tls1_2,
            self.certificates.clone(),
            None,
            self.tls_reference,
        )
    }
}

#[cfg(feature = "mbedtls")]
impl ClientTls for MbedTls {
    fn config(&self) -> Option<TlsConfig<'static>> {
        Some(self.client_config())
    }
}

/// Failure while preparing a Platform TLS capability.
#[derive(Debug, thiserror::Error)]
#[cfg(feature = "mbedtls")]
pub enum TlsError {
    /// Trust roots could not be parsed.
    #[error("invalid TLS trust roots: {0}")]
    InvalidTrustRoots(String),
    /// The process-wide mbedTLS engine could not be initialized.
    #[error("failed to initialize mbedTLS: {0}")]
    Initialize(String),
}

#[cfg(test)]
mod tests {
    use super::{ClientTls, PlaintextTls};

    #[test]
    fn plaintext_is_explicit_and_never_produces_a_tls_config() {
        assert!(PlaintextTls.config().is_none());
    }
}
