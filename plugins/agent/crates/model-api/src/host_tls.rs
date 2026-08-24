//! Process-lifetime mbedTLS state backed by the Host system trust store.

use alloc::borrow::ToOwned;
use alloc::boxed::Box;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::ffi::CStr;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use getrandom::rand_core::UnwrapErr;
use getrandom::SysRng;

use crate::{Certificate, Tls, TlsConfig, TlsReference, TlsVersion, X509};

const SYSTEM_CA_BUNDLES: &[&str] = &[
    "/etc/ssl/cert.pem",
    "/etc/ssl/certs/ca-certificates.crt",
    "/etc/pki/tls/certs/ca-bundle.crt",
    "/etc/ssl/ca-bundle.pem",
    "/opt/homebrew/etc/ca-certificates/cert.pem",
];

static HOST_TLS: OnceLock<&'static Tls<'static>> = OnceLock::new();
static HOST_TLS_INIT: Mutex<()> = Mutex::new(());

/// Reusable Host TLS configuration source for independent Model API clients.
#[derive(Clone)]
pub struct HostTls {
    certificates: Certificate<'static>,
    tls_reference: TlsReference<'static>,
}

impl HostTls {
    /// Loads the Host trust store and initializes process-lifetime mbedTLS state.
    ///
    /// `SSL_CERT_FILE`, when set, overrides the native CA bundle search paths.
    ///
    /// # Errors
    ///
    /// Returns an error when the trust store cannot be found, read, or parsed,
    /// or when mbedTLS process state cannot be initialized.
    pub fn from_system_certificates() -> Result<Self, HostTlsError> {
        Ok(Self {
            certificates: load_system_certificates()?,
            tls_reference: host_tls_reference()?,
        })
    }

    /// Creates one verified TLS configuration with the shared trust roots.
    #[must_use]
    pub fn config(&self) -> TlsConfig<'static> {
        TlsConfig::new(
            TlsVersion::Tls1_2,
            self.certificates.clone(),
            None,
            self.tls_reference,
        )
    }
}

fn load_system_certificates() -> Result<Certificate<'static>, HostTlsError> {
    if let Some(path) = std::env::var_os("SSL_CERT_FILE").filter(|value| !value.is_empty()) {
        let path = PathBuf::from(path);
        let bytes = std::fs::read(&path).map_err(|source| HostTlsError::ReadCaBundle {
            path: path.clone(),
            source,
        })?;
        return parse_certificates(path, bytes);
    }

    for candidate in SYSTEM_CA_BUNDLES {
        let path = Path::new(candidate);
        match std::fs::read(path) {
            Ok(bytes) => return parse_certificates(path.to_owned(), bytes),
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => {}
            Err(source) => {
                return Err(HostTlsError::ReadCaBundle {
                    path: path.to_owned(),
                    source,
                });
            }
        }
    }

    Err(HostTlsError::CaBundleNotFound)
}

fn parse_certificates(
    path: PathBuf,
    mut bytes: Vec<u8>,
) -> Result<Certificate<'static>, HostTlsError> {
    if bytes.contains(&0) {
        return Err(HostTlsError::InvalidCaBundle {
            path,
            message: "PEM bundle contains an embedded NUL byte".into(),
        });
    }
    bytes.push(0);
    let pem = CStr::from_bytes_with_nul(&bytes).map_err(|error| HostTlsError::InvalidCaBundle {
        path: path.clone(),
        message: error.to_string(),
    })?;
    Certificate::new(X509::PEM(pem)).map_err(|error| HostTlsError::InvalidCaBundle {
        path,
        message: error.to_string(),
    })
}

fn host_tls_reference() -> Result<TlsReference<'static>, HostTlsError> {
    let _guard = HOST_TLS_INIT
        .lock()
        .map_err(|_poisoned| HostTlsError::InitializationLock)?;
    if HOST_TLS.get().is_none() {
        let rng = Box::leak(Box::new(UnwrapErr(SysRng)));
        let tls =
            Tls::new(rng).map_err(|error| HostTlsError::Initialize(alloc::format!("{error:?}")))?;
        let tls = Box::leak(Box::new(tls));
        HOST_TLS
            .set(tls)
            .map_err(|_tls| HostTlsError::InitializationLock)?;
    }
    HOST_TLS
        .get()
        .copied()
        .map(Tls::reference)
        .ok_or(HostTlsError::InitializationLock)
}

/// Failure while preparing verified TLS for Host Model API clients.
#[derive(Debug, thiserror::Error)]
pub enum HostTlsError {
    /// No supported native CA bundle exists on this Host.
    #[error("no system CA bundle found; set SSL_CERT_FILE to a PEM certificate bundle")]
    CaBundleNotFound,
    /// The selected CA bundle could not be read.
    #[error("failed to read CA bundle {path:?}: {source}")]
    ReadCaBundle {
        /// Selected CA bundle path.
        path: PathBuf,
        /// Underlying filesystem error.
        #[source]
        source: std::io::Error,
    },
    /// The selected CA bundle was not valid PEM certificate data.
    #[error("invalid CA bundle {path:?}: {message}")]
    InvalidCaBundle {
        /// Selected CA bundle path.
        path: PathBuf,
        /// Parser failure without certificate contents.
        message: String,
    },
    /// mbedTLS could not be initialized.
    #[error("failed to initialize mbedTLS: {0}")]
    Initialize(String),
    /// Host TLS process-lifetime initialization could not be synchronized.
    #[error("failed to synchronize Host TLS initialization")]
    InitializationLock,
}
