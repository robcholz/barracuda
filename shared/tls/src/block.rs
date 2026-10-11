//! AES-128-ECB with PKCS#7 padding, and MD5, on the mbedTLS engine every
//! Platform already links.
//!
//! Some services exchange content sealed with AES-128-ECB, or name it by its
//! MD5 digest. Neither is a sound choice for new designs; they are here so a
//! caller that must speak such a service carries no second implementation.
//! Every type works incrementally, so content of any length passes through a
//! fixed amount of state.
#![allow(unsafe_code)]

use alloc::{boxed::Box, vec::Vec};

use mbedtls_rs::sys::{
    mbedtls_aes_context, mbedtls_aes_crypt_ecb, mbedtls_aes_free, mbedtls_aes_init,
    mbedtls_aes_setkey_dec, mbedtls_aes_setkey_enc, mbedtls_md5_context, mbedtls_md5_finish,
    mbedtls_md5_free, mbedtls_md5_init, mbedtls_md5_starts, mbedtls_md5_update,
    MBEDTLS_AES_DECRYPT, MBEDTLS_AES_ENCRYPT,
};

/// Bytes of one AES block.
pub const AES_BLOCK_LEN: usize = 16;
/// Bytes of an AES-128 key.
pub const AES_128_KEY_LEN: usize = 16;
/// Bytes of an MD5 digest.
pub const MD5_LEN: usize = 16;

/// Why a cipher or digest operation failed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum CryptoError {
    /// mbedTLS refused the operation.
    #[error("the crypto engine failed")]
    Engine,
    /// The ciphertext is not a whole number of blocks, or its last block
    /// does not end in valid PKCS#7 padding: wrong key or altered bytes.
    #[error("ciphertext is not validly padded")]
    Padding,
}

/// Length of the AES-128-ECB ciphertext of `plaintext_len` bytes with PKCS#7
/// padding: the next whole block, a full block more when already aligned.
#[must_use]
pub const fn aes_128_ecb_padded_len(plaintext_len: u64) -> u64 {
    let block = AES_BLOCK_LEN as u64;
    (plaintext_len / block)
        .saturating_add(1)
        .saturating_mul(block)
}

/// An AES key schedule, boxed so mbedTLS's context never moves, freed when
/// dropped.
struct Aes(Box<mbedtls_aes_context>);

impl Aes {
    fn new(key: &[u8; AES_128_KEY_LEN], encrypt: bool) -> Result<Self, CryptoError> {
        // SAFETY: an all-zero context is a valid argument to `mbedtls_aes_init`,
        // which then initializes it.
        let mut context = Self(Box::new(unsafe { core::mem::zeroed() }));
        // SAFETY: the context is exclusively borrowed and not yet initialized.
        unsafe { mbedtls_aes_init(&mut *context.0) };
        // SAFETY: the context is initialized and `key` holds 128 bits.
        let status = unsafe {
            if encrypt {
                mbedtls_aes_setkey_enc(&mut *context.0, key.as_ptr(), 128)
            } else {
                mbedtls_aes_setkey_dec(&mut *context.0, key.as_ptr(), 128)
            }
        };
        if status == 0 {
            Ok(context)
        } else {
            Err(CryptoError::Engine)
        }
    }

    fn block(
        &mut self,
        encrypt: bool,
        input: &[u8; AES_BLOCK_LEN],
    ) -> Result<[u8; AES_BLOCK_LEN], CryptoError> {
        let mut output = [0_u8; AES_BLOCK_LEN];
        let mode = if encrypt {
            MBEDTLS_AES_ENCRYPT
        } else {
            MBEDTLS_AES_DECRYPT
        };
        let mode = core::ffi::c_int::try_from(mode).map_err(|_overflow| CryptoError::Engine)?;
        // SAFETY: the context holds a key schedule for this direction, and
        // both buffers are one block long and do not overlap.
        let status = unsafe {
            mbedtls_aes_crypt_ecb(&mut *self.0, mode, input.as_ptr(), output.as_mut_ptr())
        };
        if status == 0 {
            Ok(output)
        } else {
            Err(CryptoError::Engine)
        }
    }
}

impl Drop for Aes {
    fn drop(&mut self) {
        // SAFETY: the context was initialized in `new` and is freed once.
        unsafe { mbedtls_aes_free(&mut *self.0) };
    }
}

/// Bytes held back until a whole block is known.
struct Pending {
    bytes: [u8; AES_BLOCK_LEN],
    len: usize,
}

impl Pending {
    const fn new() -> Self {
        Self {
            bytes: [0; AES_BLOCK_LEN],
            len: 0,
        }
    }

    /// Moves bytes from `input` until the block is full; returns the rest.
    fn fill<'a>(&mut self, input: &'a [u8]) -> &'a [u8] {
        let wanted = AES_BLOCK_LEN.saturating_sub(self.len).min(input.len());
        let (taken, rest) = input.split_at(wanted);
        if let Some(slot) = self
            .bytes
            .get_mut(self.len..self.len.saturating_add(wanted))
        {
            slot.copy_from_slice(taken);
        }
        self.len = self.len.saturating_add(wanted);
        rest
    }

    const fn is_full(&self) -> bool {
        self.len == AES_BLOCK_LEN
    }
}

/// Incremental AES-128-ECB encryption with PKCS#7 padding.
///
/// [`Self::update`] appends the ciphertext of every whole block it has seen;
/// [`Self::finish`] pads and appends the last block. The ciphertext is
/// [`aes_128_ecb_padded_len`] of the plaintext long.
pub struct Aes128EcbEncryptor {
    aes: Aes,
    pending: Pending,
}

impl Aes128EcbEncryptor {
    /// Starts encrypting with `key`.
    ///
    /// # Errors
    ///
    /// Returns [`CryptoError::Engine`] when mbedTLS refuses the key.
    pub fn new(key: &[u8; AES_128_KEY_LEN]) -> Result<Self, CryptoError> {
        Ok(Self {
            aes: Aes::new(key, true)?,
            pending: Pending::new(),
        })
    }

    /// Encrypts `input` and appends every completed block to `output`.
    ///
    /// # Errors
    ///
    /// Returns [`CryptoError::Engine`] when mbedTLS fails.
    pub fn update(&mut self, mut input: &[u8], output: &mut Vec<u8>) -> Result<(), CryptoError> {
        while !input.is_empty() {
            input = self.pending.fill(input);
            if self.pending.is_full() {
                let block = self.aes.block(true, &self.pending.bytes)?;
                output.extend_from_slice(&block);
                self.pending.len = 0;
            }
        }
        Ok(())
    }

    /// Pads the remaining bytes and appends the last block to `output`.
    ///
    /// # Errors
    ///
    /// Returns [`CryptoError::Engine`] when mbedTLS fails.
    pub fn finish(mut self, output: &mut Vec<u8>) -> Result<(), CryptoError> {
        let padding = AES_BLOCK_LEN.saturating_sub(self.pending.len);
        let value = u8::try_from(padding).map_err(|_overflow| CryptoError::Engine)?;
        if let Some(tail) = self.pending.bytes.get_mut(self.pending.len..) {
            tail.fill(value);
        }
        let block = self.aes.block(true, &self.pending.bytes)?;
        output.extend_from_slice(&block);
        Ok(())
    }
}

/// Incremental AES-128-ECB decryption that removes PKCS#7 padding.
///
/// The last whole block is held back, since only [`Self::finish`] knows it
/// is the one carrying the padding.
pub struct Aes128EcbDecryptor {
    aes: Aes,
    pending: Pending,
    /// The plaintext of the latest whole block, not yet released.
    held: Option<[u8; AES_BLOCK_LEN]>,
}

impl Aes128EcbDecryptor {
    /// Starts decrypting with `key`.
    ///
    /// # Errors
    ///
    /// Returns [`CryptoError::Engine`] when mbedTLS refuses the key.
    pub fn new(key: &[u8; AES_128_KEY_LEN]) -> Result<Self, CryptoError> {
        Ok(Self {
            aes: Aes::new(key, false)?,
            pending: Pending::new(),
            held: None,
        })
    }

    /// Decrypts `input` and appends the plaintext of every block known not
    /// to be the last one to `output`.
    ///
    /// # Errors
    ///
    /// Returns [`CryptoError::Engine`] when mbedTLS fails.
    pub fn update(&mut self, mut input: &[u8], output: &mut Vec<u8>) -> Result<(), CryptoError> {
        while !input.is_empty() {
            input = self.pending.fill(input);
            if self.pending.is_full() {
                let block = self.aes.block(false, &self.pending.bytes)?;
                self.pending.len = 0;
                if let Some(previous) = self.held.replace(block) {
                    output.extend_from_slice(&previous);
                }
            }
        }
        Ok(())
    }

    /// Checks and removes the padding, and appends the rest of the last
    /// block to `output`.
    ///
    /// # Errors
    ///
    /// Returns [`CryptoError::Padding`] when the ciphertext was not a whole,
    /// non-empty number of blocks or its padding is invalid; nothing is
    /// appended then.
    pub fn finish(self, output: &mut Vec<u8>) -> Result<(), CryptoError> {
        let Some(last) = self.held.filter(|_block| self.pending.len == 0) else {
            return Err(CryptoError::Padding);
        };
        let padding = last.last().copied().map_or(0, usize::from);
        if padding == 0 || padding > AES_BLOCK_LEN {
            return Err(CryptoError::Padding);
        }
        let kept = AES_BLOCK_LEN.saturating_sub(padding);
        let (text, pad) = last.split_at(kept);
        // Every padding byte carries the padding length.
        if pad.iter().any(|&byte| usize::from(byte) != padding) {
            return Err(CryptoError::Padding);
        }
        output.extend_from_slice(text);
        Ok(())
    }
}

/// Encrypts `plaintext` with AES-128-ECB and PKCS#7 padding.
///
/// # Errors
///
/// Returns [`CryptoError::Engine`] when mbedTLS fails.
pub fn aes_128_ecb_encrypt(
    key: &[u8; AES_128_KEY_LEN],
    plaintext: &[u8],
) -> Result<Vec<u8>, CryptoError> {
    let length = usize::try_from(aes_128_ecb_padded_len(plaintext.len() as u64))
        .map_err(|_overflow| CryptoError::Engine)?;
    let mut output = Vec::with_capacity(length);
    let mut encryptor = Aes128EcbEncryptor::new(key)?;
    encryptor.update(plaintext, &mut output)?;
    encryptor.finish(&mut output)?;
    Ok(output)
}

/// Decrypts `ciphertext` with AES-128-ECB and removes its PKCS#7 padding.
///
/// # Errors
///
/// Returns [`CryptoError::Padding`] for a ciphertext that is not whole
/// blocks or not validly padded (a wrong key, most often), and
/// [`CryptoError::Engine`] when mbedTLS fails.
pub fn aes_128_ecb_decrypt(
    key: &[u8; AES_128_KEY_LEN],
    ciphertext: &[u8],
) -> Result<Vec<u8>, CryptoError> {
    let mut output = Vec::with_capacity(ciphertext.len());
    let mut decryptor = Aes128EcbDecryptor::new(key)?;
    decryptor.update(ciphertext, &mut output)?;
    decryptor.finish(&mut output)?;
    Ok(output)
}

/// Incremental MD5 digest.
pub struct Md5(Box<mbedtls_md5_context>);

impl Md5 {
    /// Starts an empty digest.
    ///
    /// # Errors
    ///
    /// Returns [`CryptoError::Engine`] when mbedTLS fails.
    pub fn new() -> Result<Self, CryptoError> {
        // SAFETY: an all-zero context is a valid argument to `mbedtls_md5_init`,
        // which then initializes it.
        let mut context = Self(Box::new(unsafe { core::mem::zeroed() }));
        // SAFETY: the context is exclusively borrowed and not yet initialized;
        // `starts` then sets its initial state.
        let status = unsafe {
            mbedtls_md5_init(&mut *context.0);
            mbedtls_md5_starts(&mut *context.0)
        };
        if status == 0 {
            Ok(context)
        } else {
            Err(CryptoError::Engine)
        }
    }

    /// Adds `input` to the digest.
    ///
    /// # Errors
    ///
    /// Returns [`CryptoError::Engine`] when mbedTLS fails.
    pub fn update(&mut self, input: &[u8]) -> Result<(), CryptoError> {
        // SAFETY: the context is started and `input` holds its length.
        let status = unsafe { mbedtls_md5_update(&mut *self.0, input.as_ptr(), input.len()) };
        if status == 0 {
            Ok(())
        } else {
            Err(CryptoError::Engine)
        }
    }

    /// The digest of everything added.
    ///
    /// # Errors
    ///
    /// Returns [`CryptoError::Engine`] when mbedTLS fails.
    pub fn finish(mut self) -> Result<[u8; MD5_LEN], CryptoError> {
        let mut digest = [0_u8; MD5_LEN];
        // SAFETY: the context is started and `digest` holds 16 bytes.
        let status = unsafe { mbedtls_md5_finish(&mut *self.0, digest.as_mut_ptr()) };
        if status == 0 {
            Ok(digest)
        } else {
            Err(CryptoError::Engine)
        }
    }
}

impl Drop for Md5 {
    fn drop(&mut self) {
        // SAFETY: the context was initialized in `new` and is freed once.
        unsafe { mbedtls_md5_free(&mut *self.0) };
    }
}

/// The MD5 digest of `input`.
///
/// # Errors
///
/// Returns [`CryptoError::Engine`] when mbedTLS fails.
pub fn md5(input: &[u8]) -> Result<[u8; MD5_LEN], CryptoError> {
    let mut digest = Md5::new()?;
    digest.update(input)?;
    digest.finish()
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::indexing_slicing)]
mod tests {
    use alloc::vec::Vec;

    use super::{
        aes_128_ecb_decrypt, aes_128_ecb_encrypt, aes_128_ecb_padded_len, md5, Aes128EcbDecryptor,
        Aes128EcbEncryptor, CryptoError, Md5,
    };

    /// FIPS-197 appendix C.1: AES-128 of one block.
    const FIPS_KEY: [u8; 16] = [
        0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c, 0x0d, 0x0e,
        0x0f,
    ];
    const FIPS_PLAINTEXT: [u8; 16] = [
        0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xaa, 0xbb, 0xcc, 0xdd, 0xee,
        0xff,
    ];
    const FIPS_CIPHERTEXT: [u8; 16] = [
        0x69, 0xc4, 0xe0, 0xd8, 0x6a, 0x7b, 0x04, 0x30, 0xd8, 0xcd, 0xb7, 0x80, 0x70, 0xb4, 0xc5,
        0x5a,
    ];
    /// The FIPS-197 key encrypting a whole block of PKCS#7 padding (sixteen
    /// 0x10 bytes), which follows an aligned plaintext.
    const FIPS_PADDING_BLOCK: [u8; 16] = [
        0x95, 0x4f, 0x64, 0xf2, 0xe4, 0xe8, 0x6e, 0x9e, 0xee, 0x82, 0xd2, 0x02, 0x16, 0x68, 0x48,
        0x99,
    ];

    fn hex(bytes: &[u8]) -> alloc::string::String {
        use core::fmt::Write as _;
        let mut text = alloc::string::String::new();
        for byte in bytes {
            write!(text, "{byte:02x}").expect("write to a String");
        }
        text
    }

    #[test]
    fn encrypts_the_fips_197_block_followed_by_a_padding_block() {
        let sealed = aes_128_ecb_encrypt(&FIPS_KEY, &FIPS_PLAINTEXT).expect("encrypt");
        assert_eq!(sealed.len(), 32);
        assert_eq!(sealed[..16], FIPS_CIPHERTEXT);
        assert_eq!(sealed[16..], FIPS_PADDING_BLOCK);
        assert_eq!(
            aes_128_ecb_decrypt(&FIPS_KEY, &sealed).expect("decrypt"),
            FIPS_PLAINTEXT
        );
    }

    #[test]
    fn pads_to_the_next_whole_block() {
        assert_eq!(aes_128_ecb_padded_len(0), 16);
        assert_eq!(aes_128_ecb_padded_len(15), 16);
        assert_eq!(aes_128_ecb_padded_len(16), 32);
        assert_eq!(aes_128_ecb_padded_len(17), 32);
        for length in [0_usize, 1, 15, 16, 17, 100] {
            let plaintext: Vec<u8> = (0..length).map(|index| index as u8).collect();
            let sealed = aes_128_ecb_encrypt(&FIPS_KEY, &plaintext).expect("encrypt");
            assert_eq!(sealed.len() as u64, aes_128_ecb_padded_len(length as u64));
            assert_eq!(
                aes_128_ecb_decrypt(&FIPS_KEY, &sealed).expect("decrypt"),
                plaintext
            );
        }
    }

    #[test]
    fn streams_in_pieces_of_any_size_to_the_same_bytes() {
        let plaintext: Vec<u8> = (0..1000_u32).map(|index| (index * 7) as u8).collect();
        let whole = aes_128_ecb_encrypt(&FIPS_KEY, &plaintext).expect("encrypt");
        for piece in [1, 5, 16, 33, 999] {
            let mut sealed = Vec::new();
            let mut encryptor = Aes128EcbEncryptor::new(&FIPS_KEY).expect("key");
            for chunk in plaintext.chunks(piece) {
                encryptor.update(chunk, &mut sealed).expect("update");
            }
            encryptor.finish(&mut sealed).expect("finish");
            assert_eq!(sealed, whole, "pieces of {piece}");
            let mut opened = Vec::new();
            let mut decryptor = Aes128EcbDecryptor::new(&FIPS_KEY).expect("key");
            for chunk in sealed.chunks(piece) {
                decryptor.update(chunk, &mut opened).expect("update");
            }
            decryptor.finish(&mut opened).expect("finish");
            assert_eq!(opened, plaintext, "pieces of {piece}");
        }
    }

    #[test]
    fn refuses_a_wrong_key_partial_blocks_and_empty_input() {
        let sealed = aes_128_ecb_encrypt(&FIPS_KEY, b"seventeen bytes!!").expect("encrypt");
        let mut key = FIPS_KEY;
        key[0] ^= 1;
        assert_eq!(
            aes_128_ecb_decrypt(&key, &sealed),
            Err(CryptoError::Padding)
        );
        assert_eq!(
            aes_128_ecb_decrypt(&FIPS_KEY, &sealed[..20]),
            Err(CryptoError::Padding)
        );
        assert_eq!(
            aes_128_ecb_decrypt(&FIPS_KEY, &[]),
            Err(CryptoError::Padding)
        );
    }

    #[test]
    fn digests_the_rfc_1321_vectors() {
        let vectors: [(&[u8], &str); 4] = [
            (b"", "d41d8cd98f00b204e9800998ecf8427e"),
            (b"abc", "900150983cd24fb0d6963f7d28e17f72"),
            (b"message digest", "f96b697d7cb7938d525a2f31aaf161d0"),
            (
                b"12345678901234567890123456789012345678901234567890123456789012345678901234567890",
                "57edf4a22be3c955ac49da2e2107b67a",
            ),
        ];
        for (input, digest) in vectors {
            assert_eq!(hex(&md5(input).expect("digest")), digest);
            let mut incremental = Md5::new().expect("start");
            for chunk in input.chunks(7) {
                incremental.update(chunk).expect("update");
            }
            assert_eq!(hex(&incremental.finish().expect("finish")), digest);
        }
    }
}
