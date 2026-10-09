//! AES-256-GCM decryption on the mbedTLS engine every Platform already links.
//!
//! Callers that receive a sealed value from a service (QQ's scan-to-bind
//! returns the bot's App Secret this way) open it here rather than carry a
//! second AES implementation.
#![allow(unsafe_code)]

use alloc::{vec, vec::Vec};

use mbedtls_rs::sys::{
    mbedtls_cipher_id_t_MBEDTLS_CIPHER_ID_AES, mbedtls_gcm_auth_decrypt, mbedtls_gcm_context,
    mbedtls_gcm_free, mbedtls_gcm_init, mbedtls_gcm_setkey,
};

/// Bytes of an AES-256-GCM nonce.
pub const GCM_NONCE_LEN: usize = 12;
/// Bytes of an AES-256-GCM authentication tag.
pub const GCM_TAG_LEN: usize = 16;

/// Why a sealed value did not open.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum OpenError {
    /// The value is shorter than its tag.
    #[error("sealed value is shorter than its authentication tag")]
    Truncated,
    /// The tag does not match: wrong key, wrong nonce, or altered bytes.
    #[error("sealed value failed authentication")]
    Authentication,
}

/// Decrypts `sealed`, the ciphertext followed by its 16-byte tag, with
/// `key` and `nonce` and no associated data.
///
/// # Errors
///
/// Returns [`OpenError`] when `sealed` is shorter than a tag or does not
/// authenticate; no plaintext is returned then.
pub fn aes_256_gcm_open(
    key: &[u8; 32],
    nonce: &[u8; GCM_NONCE_LEN],
    sealed: &[u8],
) -> Result<Vec<u8>, OpenError> {
    let length = sealed
        .len()
        .checked_sub(GCM_TAG_LEN)
        .ok_or(OpenError::Truncated)?;
    let (ciphertext, tag) = sealed.split_at(length);
    let mut plaintext = vec![0_u8; length];
    let mut context = Gcm::new();
    // SAFETY: the context is initialized, the key is 256 bits, and every
    // buffer holds the length passed with it; `plaintext` does not overlap
    // `ciphertext`.
    let status = unsafe {
        let status = mbedtls_gcm_setkey(
            &mut context.0,
            mbedtls_cipher_id_t_MBEDTLS_CIPHER_ID_AES,
            key.as_ptr(),
            256,
        );
        if status == 0 {
            mbedtls_gcm_auth_decrypt(
                &mut context.0,
                length,
                nonce.as_ptr(),
                nonce.len(),
                core::ptr::null(),
                0,
                tag.as_ptr(),
                tag.len(),
                ciphertext.as_ptr(),
                plaintext.as_mut_ptr(),
            )
        } else {
            status
        }
    };
    if status == 0 {
        Ok(plaintext)
    } else {
        Err(OpenError::Authentication)
    }
}

/// A GCM context freed, with the key schedule it holds, when dropped.
struct Gcm(mbedtls_gcm_context);

impl Gcm {
    fn new() -> Self {
        // SAFETY: an all-zero context is a valid argument to `mbedtls_gcm_init`,
        // which then initializes it.
        let mut context = Self(unsafe { core::mem::zeroed() });
        // SAFETY: the context is exclusively borrowed and not yet initialized.
        unsafe { mbedtls_gcm_init(&mut context.0) };
        context
    }
}

impl Drop for Gcm {
    fn drop(&mut self) {
        // SAFETY: the context was initialized in `new` and is freed once.
        unsafe { mbedtls_gcm_free(&mut self.0) };
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::indexing_slicing)]
mod tests {
    use super::{aes_256_gcm_open, OpenError};

    /// Key and nonce of the GCM specification's test case 15.
    const KEY: [u8; 32] = [
        0xfe, 0xff, 0xe9, 0x92, 0x86, 0x65, 0x73, 0x1c, 0x6d, 0x6a, 0x8f, 0x94, 0x67, 0x30, 0x83,
        0x08, 0xfe, 0xff, 0xe9, 0x92, 0x86, 0x65, 0x73, 0x1c, 0x6d, 0x6a, 0x8f, 0x94, 0x67, 0x30,
        0x83, 0x08,
    ];
    const NONCE: [u8; 12] = [
        0xca, 0xfe, 0xba, 0xbe, 0xfa, 0xce, 0xdb, 0xad, 0xde, 0xca, 0xf8, 0x88,
    ];

    /// The GCM specification's test case 15 (AES-256, 64-byte plaintext, no
    /// associated data): ciphertext followed by its tag.
    fn sealed() -> alloc::vec::Vec<u8> {
        let mut sealed = alloc::vec::Vec::new();
        sealed.extend_from_slice(&[
            0x52, 0x2d, 0xc1, 0xf0, 0x99, 0x56, 0x7d, 0x07, 0xf4, 0x7f, 0x37, 0xa3, 0x2a, 0x84,
            0x42, 0x7d, 0x64, 0x3a, 0x8c, 0xdc, 0xbf, 0xe5, 0xc0, 0xc9, 0x75, 0x98, 0xa2, 0xbd,
            0x25, 0x55, 0xd1, 0xaa, 0x8c, 0xb0, 0x8e, 0x48, 0x59, 0x0d, 0xbb, 0x3d, 0xa7, 0xb0,
            0x8b, 0x10, 0x56, 0x82, 0x88, 0x38, 0xc5, 0xf6, 0x1e, 0x63, 0x93, 0xba, 0x7a, 0x0a,
            0xbc, 0xc9, 0xf6, 0x62, 0x89, 0x80, 0x15, 0xad,
        ]);
        sealed.extend_from_slice(&[
            0xb0, 0x94, 0xda, 0xc5, 0xd9, 0x34, 0x71, 0xbd, 0xec, 0x1a, 0x50, 0x22, 0x70, 0xe3,
            0xcc, 0x6c,
        ]);
        sealed
    }

    const PLAINTEXT: [u8; 64] = [
        0xd9, 0x31, 0x32, 0x25, 0xf8, 0x84, 0x06, 0xe5, 0xa5, 0x59, 0x09, 0xc5, 0xaf, 0xf5, 0x26,
        0x9a, 0x86, 0xa7, 0xa9, 0x53, 0x15, 0x34, 0xf7, 0xda, 0x2e, 0x4c, 0x30, 0x3d, 0x8a, 0x31,
        0x8a, 0x72, 0x1c, 0x3c, 0x0c, 0x95, 0x95, 0x68, 0x09, 0x53, 0x2f, 0xcf, 0x0e, 0x24, 0x49,
        0xa6, 0xb5, 0x25, 0xb1, 0x6a, 0xed, 0xf5, 0xaa, 0x0d, 0xe6, 0x57, 0xba, 0x63, 0x7b, 0x39,
        0x1a, 0xaf, 0xd2, 0x55,
    ];

    #[test]
    fn opens_the_specification_vector() {
        assert_eq!(
            aes_256_gcm_open(&KEY, &NONCE, &sealed()).expect("open"),
            PLAINTEXT
        );
    }

    #[test]
    fn refuses_an_altered_value_a_wrong_key_and_a_truncated_value() {
        let mut altered = sealed();
        altered[3] ^= 1;
        assert_eq!(
            aes_256_gcm_open(&KEY, &NONCE, &altered),
            Err(OpenError::Authentication)
        );
        let mut key = KEY;
        key[0] ^= 1;
        assert_eq!(
            aes_256_gcm_open(&key, &NONCE, &sealed()),
            Err(OpenError::Authentication)
        );
        assert_eq!(
            aes_256_gcm_open(&KEY, &NONCE, &[0; 15]),
            Err(OpenError::Truncated)
        );
        assert_eq!(
            aes_256_gcm_open(&KEY, &NONCE, &sealed()[64..]),
            Err(OpenError::Authentication)
        );
    }
}
