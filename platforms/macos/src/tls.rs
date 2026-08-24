//! macOS trust-root discovery and TLS initialization.

use std::path::{Path, PathBuf};

use getrandom::rand_core::UnwrapErr;

const SYSTEM_CA_BUNDLES: &[&str] = &[
    "/etc/ssl/cert.pem",
    "/opt/homebrew/etc/ca-certificates/cert.pem",
];

pub(crate) fn initialize() -> Result<barracuda_tls::MbedTls, MacosTlsError> {
    let bytes = load_system_certificates()?;
    let certificate = core::ffi::CStr::from_bytes_with_nul(&bytes)
        .map_err(|_error| MacosTlsError::InvalidCertificateBundle)?;
    let rng = Box::leak(Box::new(UnwrapErr(getrandom::SysRng)));
    barracuda_tls::MbedTls::from_pem(rng, certificate).map_err(MacosTlsError::Tls)
}

fn load_system_certificates() -> Result<Vec<u8>, MacosTlsError> {
    if let Some(path) = std::env::var_os("SSL_CERT_FILE").filter(|value| !value.is_empty()) {
        return read_bundle(PathBuf::from(path));
    }
    for candidate in SYSTEM_CA_BUNDLES {
        let path = Path::new(candidate);
        match std::fs::read(path) {
            Ok(bytes) => return terminate_pem(path, bytes),
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => {}
            Err(source) => {
                return Err(MacosTlsError::ReadCertificateBundle {
                    path: path.to_owned(),
                    source,
                });
            }
        }
    }
    Err(MacosTlsError::CertificateBundleNotFound)
}

fn read_bundle(path: PathBuf) -> Result<Vec<u8>, MacosTlsError> {
    let bytes = std::fs::read(&path).map_err(|source| MacosTlsError::ReadCertificateBundle {
        path: path.clone(),
        source,
    })?;
    terminate_pem(&path, bytes)
}

fn terminate_pem(path: &Path, mut bytes: Vec<u8>) -> Result<Vec<u8>, MacosTlsError> {
    if bytes.contains(&0) {
        return Err(MacosTlsError::EmbeddedNul {
            path: path.to_owned(),
        });
    }
    bytes.push(0);
    Ok(bytes)
}

/// macOS trust-store or TLS-engine initialization failure.
#[derive(Debug, thiserror::Error)]
pub enum MacosTlsError {
    /// No supported macOS CA bundle exists.
    #[error("no macOS CA bundle found; set SSL_CERT_FILE to a PEM certificate bundle")]
    CertificateBundleNotFound,
    /// A macOS CA bundle could not be read.
    #[error("failed to read macOS CA bundle {path:?}: {source}")]
    ReadCertificateBundle {
        /// Selected bundle path.
        path: PathBuf,
        /// Underlying filesystem error.
        #[source]
        source: std::io::Error,
    },
    /// A PEM bundle contained an interior NUL byte.
    #[error("macOS CA bundle {path:?} contains an embedded NUL byte")]
    EmbeddedNul {
        /// Invalid bundle path.
        path: PathBuf,
    },
    /// The prepared bundle was not a single terminated C string.
    #[error("macOS CA bundle is not a valid terminated PEM byte string")]
    InvalidCertificateBundle,
    /// Shared TLS engine initialization failed.
    #[error(transparent)]
    Tls(#[from] barracuda_tls::TlsError),
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::{terminate_pem, MacosTlsError};

    #[test]
    fn macos_bundle_is_terminated_for_mbedtls() -> Result<(), MacosTlsError> {
        let bytes = terminate_pem(Path::new("roots.pem"), b"pem".to_vec())?;
        assert_eq!(bytes, b"pem\0");
        Ok(())
    }

    #[test]
    fn macos_bundle_rejects_embedded_nul() {
        let result = terminate_pem(Path::new("roots.pem"), b"bad\0pem".to_vec());
        assert!(matches!(result, Err(MacosTlsError::EmbeddedNul { .. })));
    }
}
