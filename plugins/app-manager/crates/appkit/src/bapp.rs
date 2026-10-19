//! App packages: a container's files in one `.bapp` file, which its author
//! may sign. `docs/plugins/app-manager/app-format.mdx` defines the layout.
//!
//! Listing reads only a package's header and directory. The digest, which
//! pins what the person approved, and the signature are checked only when
//! an install is asked for and when it happens.

use alloc::{format, string::String, vec, vec::Vec};
use barracuda_vfs::{FsError, ScopedVfs};
use embedded_io_async::Read as _;
use p256::ecdsa::{Signature, VerifyingKey, signature::DigestVerifier};
use sha2::{Digest as _, Sha256};

/// The file extension of an app package.
pub const PACKAGE_EXTENSION: &str = ".bapp";
/// The most files a package holds.
pub const MAX_PACKAGE_FILES: usize = 64;
/// The largest package `AppManager::offer_package` takes: well above a
/// valid one, whose manifest, program and view files come to at most 580 KiB.
pub const MAX_PACKAGE_BYTES: u64 = 1024 * 1024;

const MAGIC: &[u8; 4] = b"BAPP";
const SIGNATURE_MAGIC: &[u8; 4] = b"BSIG";
const VERSION: u8 = 1;
/// Flags bit: a signature block follows the signed content.
const SIGNED: u8 = 1;
/// Magic, version, file count, flags, reserved, and the content's length.
const HEADER_BYTES: usize = 12;
/// A file's NUL-padded name, its offset, and its length.
const ENTRY_BYTES: usize = 40;
const NAME_BYTES: usize = 32;
/// The signature block's magic, SEC1 uncompressed P-256 public key, and
/// `r || s` signature.
const SIGNATURE_BYTES: usize = 4 + PUBLIC_KEY_BYTES + 64;
/// A SEC1 uncompressed P-256 public key.
pub const PUBLIC_KEY_BYTES: usize = 65;
/// How much of a package is hashed at a time.
const CHUNK_BYTES: usize = 4096;

/// Why a package cannot be read.
#[derive(Debug)]
pub(crate) enum PackageError {
    /// The filesystem failed.
    Storage,
    /// The package breaks the format; the message says how.
    Invalid(String),
}

impl From<FsError> for PackageError {
    fn from(_error: FsError) -> Self {
        Self::Storage
    }
}

fn invalid(message: impl Into<String>) -> PackageError {
    PackageError::Invalid(message.into())
}

struct PackedFile {
    name: String,
    offset: u64,
    len: u64,
}

/// A package's directory: which files it holds and where.
pub(crate) struct Package {
    path: String,
    files: Vec<PackedFile>,
    /// The bytes the digest and signature cover: the header, the directory,
    /// and the files.
    content_len: u64,
    signed: bool,
}

/// What a package's content hashes to, and who signed it, checked.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Seal {
    /// SHA-256 of the content.
    pub digest: [u8; 32],
    /// The SEC1 public key whose signature over the content verified, or
    /// `None` for an unsigned package.
    pub signer: Option<[u8; PUBLIC_KEY_BYTES]>,
}

impl Package {
    /// Reads the header and directory of the package at `path`.
    pub(crate) async fn open(filesystem: &ScopedVfs, path: &str) -> Result<Self, PackageError> {
        let size = filesystem.len(path).await?;
        if size < HEADER_BYTES as u64 {
            return Err(invalid("the package is shorter than its header"));
        }
        let header = filesystem.read_at(path, 0, HEADER_BYTES).await?;
        if &header[..4] != MAGIC || header[4] != VERSION {
            return Err(invalid("the file is not a version 1 app package"));
        }
        let count = usize::from(header[5]);
        let signed = header[6] & SIGNED != 0;
        let content_len = u64::from(u32::from_le_bytes([
            header[8], header[9], header[10], header[11],
        ]));
        if count == 0 || count > MAX_PACKAGE_FILES {
            return Err(invalid(format!(
                "a package holds 1 to {MAX_PACKAGE_FILES} files"
            )));
        }
        let expected = content_len + if signed { SIGNATURE_BYTES as u64 } else { 0 };
        if size != expected {
            return Err(invalid("the package's length does not match its header"));
        }
        let directory_end = (HEADER_BYTES + count * ENTRY_BYTES) as u64;
        let directory = filesystem
            .read_at(path, HEADER_BYTES as u64, count * ENTRY_BYTES)
            .await?;
        let mut files: Vec<PackedFile> = Vec::with_capacity(count);
        for entry in directory.as_chunks::<ENTRY_BYTES>().0 {
            let name = &entry[..NAME_BYTES];
            let end = name
                .iter()
                .position(|byte| *byte == 0)
                .unwrap_or(NAME_BYTES);
            let name = core::str::from_utf8(&name[..end])
                .map_err(|_| invalid("a file name in the package is not UTF-8"))?;
            let offset = u64::from(u32::from_le_bytes([
                entry[32], entry[33], entry[34], entry[35],
            ]));
            let len = u64::from(u32::from_le_bytes([
                entry[36], entry[37], entry[38], entry[39],
            ]));
            if offset < directory_end || offset + len > content_len {
                return Err(invalid(format!("{name} lies outside the package")));
            }
            if files.iter().any(|file| file.name == name) {
                return Err(invalid(format!("{name} is in the package twice")));
            }
            files.push(PackedFile {
                name: String::from(name),
                offset,
                len,
            });
        }
        Ok(Self {
            path: String::from(path),
            files,
            content_len,
            signed,
        })
    }

    /// Whether the package holds file `name`.
    pub(crate) fn has(&self, name: &str) -> bool {
        self.file(name).is_some()
    }

    fn file(&self, name: &str) -> Option<&PackedFile> {
        self.files.iter().find(|file| file.name == name)
    }

    /// Reads file `name`, refusing one larger than `limit` bytes.
    pub(crate) async fn read(
        &self,
        filesystem: &ScopedVfs,
        name: &str,
        limit: u64,
    ) -> Result<Vec<u8>, PackageError> {
        let file = self
            .file(name)
            .ok_or_else(|| invalid(format!("{name} is missing")))?;
        if file.len > limit {
            return Err(invalid(format!("{name} is larger than {limit} bytes")));
        }
        let len = usize::try_from(file.len).map_err(|_| invalid("a file is too large"))?;
        Ok(filesystem.read_at(&self.path, file.offset, len).await?)
    }

    /// Hashes the content and verifies the signature, if the package has one.
    pub(crate) async fn seal(&self, filesystem: &ScopedVfs) -> Result<Seal, PackageError> {
        let mut file = filesystem.open(&self.path).await?;
        let mut hasher = Sha256::new();
        let mut chunk = vec![0; CHUNK_BYTES];
        let mut left = self.content_len;
        while left > 0 {
            let want = usize::try_from(left.min(CHUNK_BYTES as u64)).unwrap_or(CHUNK_BYTES);
            let read = file
                .read(&mut chunk[..want])
                .await
                .map_err(|_| PackageError::Storage)?;
            if read == 0 {
                return Err(invalid("the package ends early"));
            }
            hasher.update(&chunk[..read]);
            left -= read as u64;
        }
        let digest: [u8; 32] = hasher.clone().finalize().into();
        if !self.signed {
            return Ok(Seal {
                digest,
                signer: None,
            });
        }
        let block = filesystem
            .read_at(&self.path, self.content_len, SIGNATURE_BYTES)
            .await?;
        if &block[..4] != SIGNATURE_MAGIC {
            return Err(invalid("the package's signature block is damaged"));
        }
        let mut key = [0; PUBLIC_KEY_BYTES];
        key.copy_from_slice(&block[4..4 + PUBLIC_KEY_BYTES]);
        let verifying = VerifyingKey::from_sec1_bytes(&key)
            .map_err(|_| invalid("the signing key is not a P-256 key"))?;
        let signature = Signature::from_slice(&block[4 + PUBLIC_KEY_BYTES..])
            .map_err(|_| invalid("the signature is malformed"))?;
        verifying
            .verify_digest(hasher, &signature)
            .map_err(|_| invalid("the signature does not match the package"))?;
        Ok(Seal {
            digest,
            signer: Some(key),
        })
    }
}

/// Lowercase hexadecimal of `bytes`.
pub(crate) fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut text = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        text.push(char::from(DIGITS[usize::from(byte >> 4)]));
        text.push(char::from(DIGITS[usize::from(byte & 0xf)]));
    }
    text
}

/// A signer's short name: the first 8 bytes of its key's SHA-256, in hex.
pub(crate) fn fingerprint(key: &[u8; PUBLIC_KEY_BYTES]) -> String {
    hex(&Sha256::digest(key)[..8])
}
