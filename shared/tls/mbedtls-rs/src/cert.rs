use core::ffi::CStr;
use core::marker::PhantomData;

use super::sys::*;
use super::{MRc, SessionError};

/// Holds a reference to a PEM or DER-encoded X509 certificate or private key.
///
/// # Examples
/// Initialize with a PEM certificate
/// ```
/// let x509 = X509::PEM(CStr::from_bytes_with_nul(concat!(include_str!("cert.pem"), "\0").as_bytes()).unwrap());
/// ```
///
/// Initialize with a DER certificate
/// ```
/// let x509 = X509::DER(include_bytes!("cert.der"));
/// ```
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub enum X509<'a> {
    PEM(&'a CStr),
    DER(&'a [u8]),
}

/// Decides trust for one certificate of a peer's chain, as
/// `mbedtls_ssl_conf_verify` calls it: the context pointer (the session's
/// [`VerifyScratch`]), the certificate, its depth in the chain, and the
/// verification flags it may clear or set. MbedTLS calls it once per
/// certificate of the verified path, from the top down to the peer's own.
/// Returns 0, or an MbedTLS error that aborts the handshake.
pub type VerifyCallback = unsafe extern "C" fn(
    *mut core::ffi::c_void,
    *mut mbedtls_x509_crt,
    core::ffi::c_int,
    *mut u32,
) -> core::ffi::c_int;

/// One word a [`VerifyCallback`] may keep between its calls for one chain,
/// one per session and zero when the session starts.
#[derive(Debug, Default)]
pub struct VerifyScratch(pub core::cell::Cell<core::ffi::c_int>);

impl crate::MInit for VerifyScratch {}

/// A parsed X509 certificate or certificate chain.
#[derive(Debug, Clone)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub struct Certificate<'d> {
    pub(crate) crt: MRc<mbedtls_x509_crt>,
    /// Installed with `mbedtls_ssl_conf_verify` when this is a CA chain.
    #[cfg_attr(feature = "defmt", defmt(Debug2Format))]
    pub(crate) verify: Option<VerifyCallback>,
    _t: PhantomData<&'d ()>,
}

impl Certificate<'static> {
    /// An empty CA chain whose trust decisions are made by `verify`.
    ///
    /// MbedTLS finds no trusted parent in the empty chain, so the top of every
    /// peer chain arrives at `verify` flagged `MBEDTLS_X509_BADCERT_NOT_TRUSTED`;
    /// `verify` clears the flag when it trusts that certificate's issuer.
    ///
    /// # Errors
    ///
    /// This will return an error if the empty chain cannot be allocated.
    pub fn verified_by(verify: VerifyCallback) -> Result<Self, MbedtlsError> {
        let crt: MRc<mbedtls_x509_crt> =
            MRc::new().ok_or(MbedtlsError::new(MBEDTLS_ERR_X509_ALLOC_FAILED))?;

        Ok(Self {
            crt,
            verify: Some(verify),
            _t: PhantomData,
        })
    }

    /// Parse an X509 certificate into RAM by making a copy
    ///
    /// # Arguments
    ///
    /// * `certificate` - The X509 certificate in PEM or DER format
    ///
    /// # Errors
    ///
    /// This will return an error if an error occurs during parsing such as passing a DER encoded
    /// certificate in a PEM format, and vice-versa.
    pub fn new(x509: X509<'_>) -> Result<Self, MbedtlsError> {
        let mut crt: MRc<mbedtls_x509_crt> =
            MRc::new().ok_or(MbedtlsError::new(MBEDTLS_ERR_X509_ALLOC_FAILED))?;

        match x509 {
            X509::PEM(str) => merr!(unsafe {
                mbedtls_x509_crt_parse(
                    crt.as_mut_ptr(),
                    str.as_ptr() as *const _,
                    str.count_bytes() + 1,
                )
            }),
            X509::DER(bytes) => merr!(unsafe {
                mbedtls_x509_crt_parse_der(crt.as_mut_ptr(), bytes.as_ptr(), bytes.len())
            }),
        }?;

        Ok(Self {
            crt,
            verify: None,
            _t: PhantomData,
        })
    }
}

impl<'d> Certificate<'d> {
    /// Parse an X509 certificate without making a copy in RAM. This requires that the underlying data
    /// lives for the lifetime of the certificate.
    /// Note: This is currently only supported for DER encoded certificates
    ///
    /// # Arguments
    ///
    /// * `certificate` - The X509 certificate in DER format only
    ///
    /// # Errors
    ///
    /// This will return an error if an error occurs during parsing.
    /// [TlsError::InvalidFormat] will be returned if a PEM encoded certificate is passed.
    pub fn new_no_copy(x509_der: &'d [u8]) -> Result<Self, SessionError> {
        let mut crt: MRc<mbedtls_x509_crt> =
            MRc::new().ok_or(MbedtlsError::new(MBEDTLS_ERR_X509_ALLOC_FAILED))?;

        merr!(unsafe {
            mbedtls_x509_crt_parse_der_nocopy(crt.as_mut_ptr(), x509_der.as_ptr(), x509_der.len())
        })?;

        Ok(Self {
            crt,
            verify: None,
            _t: PhantomData,
        })
    }
}

/// A parsed private key
#[derive(Debug, Clone)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub struct PrivateKey(pub(crate) MRc<mbedtls_pk_context>);

impl PrivateKey {
    /// Parse an X509 private key into RAM and returns a wrapped pointer if successful.
    ///
    /// # Arguments
    ///
    /// * `private_key` - The X509 private key in DER or PEM format
    /// * `password` - The optional password if the private key is password protected
    ///
    /// # Errors
    ///
    /// This will return an error if an error occurs during parsing such as passing a DER encoded
    /// private key in a PEM format, and vice-versa.
    pub fn new(x509: X509<'_>, password: Option<&str>) -> Result<Self, SessionError> {
        let mut pk: MRc<mbedtls_pk_context> =
            MRc::new().ok_or(MbedtlsError::new(MBEDTLS_ERR_PK_ALLOC_FAILED))?;

        #[allow(clippy::unnecessary_cast)]
        let (ptr, len) = match x509 {
            X509::PEM(str) => (str.as_ptr() as *const u8, str.count_bytes() + 1),
            X509::DER(bytes) => (bytes.as_ptr(), bytes.len()),
        };

        let (password_ptr, password_len) = if let Some(password) = password {
            (password.as_ptr(), password.len())
        } else {
            (core::ptr::null(), 0)
        };

        merr!(unsafe {
            mbedtls_pk_parse_key(
                pk.as_mut_ptr(),
                ptr as _,
                len,
                password_ptr,
                password_len,
                None,
                core::ptr::null_mut(),
            )
        })?;

        Ok(Self(pk))
    }
}
