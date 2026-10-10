//! TLS client engine shared by every Platform.
//!
//! System builds one [`Tls`] from the Platform's entropy source. The trust
//! roots are Mozilla's CA store, compiled in as a compact bundle, so every
//! Platform trusts the same roots. The HTTP client turns the handle into
//! per-connection configuration.

#![no_std]

extern crate alloc;

mod aead;
#[cfg(target_os = "none")]
mod c_runtime;
mod memory;
mod roots;

use alloc::{boxed::Box, format, string::String};
use core::ffi::CStr;

use barracuda_platform::{Entropy, EntropyUnavailable};
use mbedtls_rs::{
    io::{Read, Write},
    AuthMode, ClientSessionConfig, RngFailure, SessionConfig, TlsRng,
};

pub use aead::{aes_256_gcm_open, OpenError, GCM_NONCE_LEN, GCM_TAG_LEN};
pub use mbedtls_rs::{Certificate, Session, SessionError, Split, TlsReference, TlsVersion};

/// The process-wide TLS engine and the roots it trusts.
///
/// mbedTLS allows one engine per process, so [`Tls::new`] succeeds once.
#[derive(Clone)]
pub struct Tls {
    certificates: Certificate<'static>,
    tls_reference: TlsReference<'static>,
}

impl Tls {
    /// Initializes the TLS engine from the Platform's entropy source.
    ///
    /// # Errors
    ///
    /// Returns an error when the Platform has no entropy source or the engine
    /// is already initialized.
    pub fn new<E: Entropy + Send>(entropy: E) -> Result<Self, TlsError> {
        // Refuse a source that cannot produce bytes now; one that fails later
        // fails the handshake that asked for them.
        entropy
            .fill(&mut [0; 1])
            .map_err(|EntropyUnavailable| TlsError::EntropyUnavailable)?;
        memory::install();
        let certificates = Certificate::verified_by(roots::verify)
            .map_err(|error| TlsError::Initialize(format!("{error:?}")))?;
        let rng = Box::leak(Box::new(EntropyRng(entropy)));
        let tls = mbedtls_rs::Tls::new(rng)
            .map_err(|error| TlsError::Initialize(format!("{error:?}")))?;
        let tls = Box::leak(Box::new(tls));
        log::info!("TLS trusts {} compiled-in roots", roots::count());
        Ok(Self {
            certificates,
            tls_reference: tls.reference(),
        })
    }

    /// The minimum protocol version for client connections.
    #[must_use]
    pub fn version(&self) -> TlsVersion {
        TlsVersion::Tls1_2
    }

    /// The roots that server certificate chains must lead to.
    #[must_use]
    pub fn certificates(&self) -> Certificate<'static> {
        self.certificates.clone()
    }

    /// The engine that client connections run on.
    #[must_use]
    pub fn reference(&self) -> TlsReference<'static> {
        self.tls_reference
    }

    /// Starts a client session over `stream` that verifies the server
    /// against the bundled roots and `server_name`, with the settings every
    /// HTTP client uses. [`Session::connect`] runs the handshake.
    ///
    /// # Errors
    ///
    /// Returns an error when mbedTLS cannot allocate or configure the session.
    pub fn client_session<T: Read + Write>(
        &self,
        stream: T,
        server_name: &CStr,
    ) -> Result<Session<'static, T>, SessionError> {
        let config = SessionConfig::Client(ClientSessionConfig {
            ca_chain: Some(self.certificates()),
            creds: None,
            server_name: None,
            auth_mode: AuthMode::Required,
            min_version: self.version(),
            alpn_protocols: None,
        });
        let mut session = Session::new(self.tls_reference, stream, &config)?;
        session.set_server_name(server_name)?;
        Ok(session)
    }
}

/// The Platform entropy source as the random source mbedTLS draws from.
struct EntropyRng<E>(E);

impl<E: Entropy + Send> TlsRng for EntropyRng<E> {
    fn try_fill(&mut self, bytes: &mut [u8]) -> Result<(), RngFailure> {
        self.0.fill(bytes).map_err(|EntropyUnavailable| RngFailure)
    }
}

/// Failure while initializing the TLS engine.
#[derive(Debug, thiserror::Error)]
pub enum TlsError {
    /// The Platform has no entropy source.
    #[error("the Platform has no entropy source for TLS")]
    EntropyUnavailable,
    /// The process-wide mbedTLS engine could not be initialized.
    #[error("failed to initialize mbedTLS: {0}")]
    Initialize(String),
}

#[cfg(test)]
#[allow(unsafe_code, clippy::expect_used)]
mod tests {
    extern crate std;

    // The host implementation mbedtls-rs locks its random source with.
    use critical_section as _;

    use core::ptr::{null, null_mut};
    use std::sync::Once;

    use barracuda_platform::{Entropy, EntropyUnavailable, UnavailableEntropy};
    use mbedtls_rs::sys::{
        mbedtls_pk_context, mbedtls_pk_free, mbedtls_pk_init, mbedtls_pk_parse_public_key,
        mbedtls_x509_crt, mbedtls_x509_crt_free, mbedtls_x509_crt_init, mbedtls_x509_crt_parse_der,
        mbedtls_x509_crt_verify, psa_crypto_init, MBEDTLS_X509_BADCERT_NOT_TRUSTED,
    };

    use super::{roots, Tls, TlsError};

    /// Signed by ISRG Root X1 (RSA).
    const R11: &[u8] = include_bytes!("../tests/fixtures/r11.der");
    /// Signed by ISRG Root X2 (ECDSA).
    const E5: &[u8] = include_bytes!("../tests/fixtures/e5.der");
    const ISRG_ROOT_X1: &[u8] = include_bytes!("../tests/fixtures/isrg-root-x1.der");
    const ISRG_ROOT_X2: &[u8] = include_bytes!("../tests/fixtures/isrg-root-x2.der");
    /// Self-signed, and not in the bundle.
    const UNKNOWN_ROOT: &[u8] = include_bytes!("../tests/fixtures/unknown-root.der");
    /// Signed by GTS Root R4 (ECDSA); served by openrouter.ai.
    const WE1: &[u8] = include_bytes!("../tests/fixtures/we1.der");
    /// GTS Root R4, a bundled root, cross-signed by GlobalSign Root CA, which
    /// Mozilla no longer trusts for websites.
    const GTS_ROOT_R4_CROSS: &[u8] = include_bytes!("../tests/fixtures/gts-root-r4-cross.der");
    /// bots.qq.com's chain as Tencent serves it (seen 2026-10-10): the leaf,
    /// its intermediate, GlobalSign Root CA - R3 (a bundled root)
    /// cross-signed by GlobalSign Root CA, and that older root itself, which
    /// Mozilla no longer trusts for websites.
    const BOTS_QQ_COM: &[u8] = include_bytes!("../tests/fixtures/bots-qq-com.der");
    const GLOBALSIGN_ATLAS_R3_OV: &[u8] =
        include_bytes!("../tests/fixtures/globalsign-atlas-r3-ov.der");
    const GLOBALSIGN_R3_CROSS: &[u8] = include_bytes!("../tests/fixtures/globalsign-r3-cross.der");
    const GLOBALSIGN_ROOT_CA: &[u8] = include_bytes!("../tests/fixtures/globalsign-root-ca.der");
    /// `localhost`, signed by a test intermediate whose root is in no bundle.
    const TEST_LOCALHOST: &[u8] = include_bytes!("../tests/fixtures/test-localhost.der");
    const TEST_INTERMEDIATE: &[u8] = include_bytes!("../tests/fixtures/test-intermediate.der");

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

    /// Starts the one engine this test process may have.
    fn engine() {
        static START: Once = Once::new();
        START.call_once(|| {
            let tls = Tls::new(CountingEntropy).expect("start the TLS engine");
            core::mem::forget(tls);
            // SAFETY: the engine's random source is installed.
            assert_eq!(unsafe { psa_crypto_init() }, 0);
        });
    }

    /// The flags left after verifying `chain` (leaf first) against the bundle
    /// alone, as a handshake does.
    fn verify(chain: &[&[u8]]) -> u32 {
        engine();
        // SAFETY: both chains are initialized before use and freed after.
        unsafe {
            let mut certificates = core::mem::zeroed::<mbedtls_x509_crt>();
            let mut trusted = core::mem::zeroed::<mbedtls_x509_crt>();
            mbedtls_x509_crt_init(&mut certificates);
            mbedtls_x509_crt_init(&mut trusted);
            for der in chain {
                assert_eq!(
                    mbedtls_x509_crt_parse_der(&mut certificates, der.as_ptr(), der.len()),
                    0
                );
            }
            let mut flags = 0;
            // a session's scratch, as a handshake passes it
            let scratch = mbedtls_rs::VerifyScratch::default();
            let _ = mbedtls_x509_crt_verify(
                &mut certificates,
                &mut trusted,
                null_mut(),
                null(),
                &mut flags,
                Some(roots::verify),
                core::ptr::from_ref(&scratch).cast_mut().cast(),
            );
            assert_eq!(scratch.0.get(), 0, "every pass leaves the scratch settled");
            mbedtls_x509_crt_free(&mut certificates);
            mbedtls_x509_crt_free(&mut trusted);
            flags
        }
    }

    #[test]
    fn missing_entropy_is_refused_before_the_engine_starts() {
        let result = Tls::new(UnavailableEntropy);
        assert!(matches!(result, Err(TlsError::EntropyUnavailable)));
    }

    #[test]
    fn the_engine_starts_once() {
        engine();
        assert!(matches!(
            Tls::new(CountingEntropy),
            Err(TlsError::Initialize(_))
        ));
    }

    #[test]
    fn every_root_in_the_bundle_has_a_usable_key() {
        engine();
        let roots: std::vec::Vec<_> = roots::roots().collect();
        assert_eq!(roots.len(), roots::count());
        assert!(roots.len() > 100, "only {} roots", roots.len());
        assert!(roots.windows(2).all(|pair| pair[0].name <= pair[1].name));
        for root in roots {
            // SAFETY: the context is initialized before use and freed after.
            unsafe {
                let mut key = core::mem::zeroed::<mbedtls_pk_context>();
                mbedtls_pk_init(&mut key);
                let parsed =
                    mbedtls_pk_parse_public_key(&mut key, root.key.as_ptr(), root.key.len());
                mbedtls_pk_free(&mut key);
                assert_eq!(parsed, 0);
            }
        }
    }

    #[test]
    fn intermediates_signed_by_bundled_roots_are_trusted() {
        assert_eq!(verify(&[R11]), 0, "RSA");
        assert_eq!(verify(&[E5]), 0, "ECDSA");
    }

    #[test]
    fn a_bundled_root_sent_by_the_server_is_trusted() {
        assert_eq!(verify(&[ISRG_ROOT_X1]), 0);
        assert_eq!(verify(&[R11, ISRG_ROOT_X1]), 0);
        assert_eq!(verify(&[ISRG_ROOT_X2]), 0);
    }

    #[test]
    fn a_bundled_root_cross_signed_by_an_unbundled_one_is_trusted() {
        assert_eq!(verify(&[GTS_ROOT_R4_CROSS]), 0);
        assert_eq!(verify(&[WE1, GTS_ROOT_R4_CROSS]), 0);
    }

    #[test]
    fn a_chain_reaching_a_bundled_root_is_trusted_whatever_the_server_sends_above_it() {
        let chain = [
            BOTS_QQ_COM,
            GLOBALSIGN_ATLAS_R3_OV,
            GLOBALSIGN_R3_CROSS,
            GLOBALSIGN_ROOT_CA,
        ];
        assert_eq!(verify(&chain), 0);
        assert_eq!(verify(&chain[1..]), 0);
        // the unbundled root alone, or above a chain that never reaches a
        // bundled root, is still not trusted
        assert_eq!(
            verify(&[GLOBALSIGN_ROOT_CA]) & MBEDTLS_X509_BADCERT_NOT_TRUSTED,
            MBEDTLS_X509_BADCERT_NOT_TRUSTED
        );
    }

    #[test]
    fn a_bundled_root_off_the_verified_path_vouches_for_nothing() {
        // The intermediate's root is not sent; the bundled root beside it is
        // not its parent, so mbedTLS never puts it on the path.
        assert_eq!(
            verify(&[TEST_LOCALHOST, TEST_INTERMEDIATE, ISRG_ROOT_X1])
                & MBEDTLS_X509_BADCERT_NOT_TRUSTED,
            MBEDTLS_X509_BADCERT_NOT_TRUSTED
        );
        assert_eq!(
            verify(&[
                TEST_LOCALHOST,
                TEST_INTERMEDIATE,
                GLOBALSIGN_R3_CROSS,
                GLOBALSIGN_ROOT_CA
            ]) & MBEDTLS_X509_BADCERT_NOT_TRUSTED,
            MBEDTLS_X509_BADCERT_NOT_TRUSTED
        );
    }

    #[test]
    fn a_tampered_signature_is_not_trusted() {
        let mut tampered = R11.to_vec();
        let last = tampered.len() - 1;
        tampered[last] ^= 1;
        assert_eq!(
            verify(&[&tampered]) & MBEDTLS_X509_BADCERT_NOT_TRUSTED,
            MBEDTLS_X509_BADCERT_NOT_TRUSTED
        );
    }

    #[test]
    fn a_root_outside_the_bundle_is_not_trusted() {
        assert_eq!(
            verify(&[UNKNOWN_ROOT]) & MBEDTLS_X509_BADCERT_NOT_TRUSTED,
            MBEDTLS_X509_BADCERT_NOT_TRUSTED
        );
    }
}
