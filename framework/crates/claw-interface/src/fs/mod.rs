//! The `ClawFs` filesystem injection trait.
//!
//! This is the persistence seam for everything that has to survive a reboot
//! (conversation tapes, profile/long-term memory, …). Like [`ClawHttp`], it is a
//! dependency-injection point: the espidf wiring implements it over the DATA
//! root (FATFS / SD card), while host tests provide `std::fs` or an in-memory
//! map. Modules never touch `std::fs` directly so they stay portable.
//!
//! # Two layers: a filesystem backend that produces file handles
//!
//! The seam is shaped like a statically dispatched filesystem handle:
//! - [`ClawFs`] is the platform filesystem backend selected by type and owned by
//!   each storage component. It locates
//!   paths and produces handles ([`open`](ClawFs::open) /
//!   [`create`](ClawFs::create) / [`open_append`](ClawFs::open_append)) and
//!   performs whole-path operations that have no handle (`rename`, `remove`,
//!   `list_dir`, …).
//! - [`ClawFile`] is an *open handle*: read/seek/write against one file without
//!   reopening it. Callers that touch the same file repeatedly (e.g. loading a
//!   conversation log record-by-record) hold one handle and seek within it,
//!   instead of reopening the file per access.
//!
//! For the common one-shot cases, [`ClawFs`] provides path-addressed
//! conveniences ([`read`](ClawFs::read), [`read_at`](ClawFs::read_at),
//! [`append`](ClawFs::append), [`write_atomic`](ClawFs::write_atomic), …) as
//! default methods implemented over the handle primitives, so callers that do
//! not need a persistent handle keep a terse API.
//!
//! Paths are byte-oriented, opaque strings already resolved against the DATA
//! root by the caller (`claw_paths`); this trait does no path joining.
//!
//! [`ClawHttp`]: crate::http::ClawHttp

use std::error::Error;
use std::fmt;
use std::sync::Arc;

#[cfg(feature = "diskfs")]
mod disk;
mod mem;

#[cfg(feature = "diskfs")]
pub use disk::{DiskFile, DiskFs};
pub use mem::{MemFile, MemFs};

/// Filesystem failure.
///
/// Deliberately coarse: callers either retry, log, or fall back to an empty
/// state, so the only distinction that matters is "the file isn't there" versus
/// "the underlying I/O failed". The `esp_err_t` mapping for the C ABI lives in
/// `claw_capi`.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum FsError {
    #[error("path not found")]
    NotFound,
    #[error("filesystem io error: {0}")]
    Io(#[from] FsIoError),
}

impl FsError {
    /// Preserve a concrete filesystem-adjacent failure.
    pub fn io(error: impl Error + Send + Sync + 'static) -> Self {
        Self::Io(FsIoError::new(error))
    }

    /// Build an I/O error for synthetic filesystem failures that have no lower
    /// source error.
    pub fn io_message(message: impl Into<String>) -> Self {
        Self::Io(FsIoError::message(message))
    }
}

impl From<std::io::Error> for FsError {
    fn from(error: std::io::Error) -> Self {
        if error.kind() == std::io::ErrorKind::NotFound {
            Self::NotFound
        } else {
            Self::io(error)
        }
    }
}

/// Source-preserving filesystem I/O failure.
#[derive(Debug, Clone)]
pub struct FsIoError {
    source: Arc<dyn Error + Send + Sync + 'static>,
}

impl FsIoError {
    /// Preserve a concrete underlying error.
    pub fn new(error: impl Error + Send + Sync + 'static) -> Self {
        Self {
            source: Arc::new(error),
        }
    }

    /// Create a synthetic source for filesystem failures raised by validation
    /// logic rather than a lower I/O API.
    pub fn message(message: impl Into<String>) -> Self {
        Self::new(FsMessageError(message.into()))
    }
}

impl fmt::Display for FsIoError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.source.fmt(formatter)
    }
}

impl Error for FsIoError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        Some(self.source.as_ref())
    }
}

impl PartialEq for FsIoError {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.source, &other.source)
            || match (
                self.source.as_ref().downcast_ref::<FsMessageError>(),
                other.source.as_ref().downcast_ref::<FsMessageError>(),
            ) {
                (Some(left), Some(right)) => left == right,
                _ => false,
            }
    }
}

impl Eq for FsIoError {}

#[derive(Debug, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
struct FsMessageError(String);

/// An open file handle produced by a [`ClawFs`].
///
/// One handle addresses one file without reopening it: the read cursor advances
/// across [`read_to_end`](ClawFile::read_to_end), and [`read_exact_at`] seeks to
/// an absolute offset. This is the primitive the append-only journals rely on to
/// fetch records by indexed `(offset, len)` while holding the file open once.
///
/// [`read_exact_at`]: ClawFile::read_exact_at
pub trait ClawFile {
    /// Read from the current cursor to end of file.
    fn read_to_end(&mut self) -> Result<Vec<u8>, FsError>;

    /// Seek to absolute byte `offset` and read exactly `len` bytes.
    ///
    /// Used to fetch a single record out of an append-only log via its indexed
    /// `(offset, len)`. An `offset`/`len` past the end of the file is an
    /// [`FsError::Io`] (a corrupt/short file), not a silent truncation:
    /// implementations return exactly `len` bytes on success.
    fn read_exact_at(&mut self, offset: u64, len: usize) -> Result<Vec<u8>, FsError>;

    /// Byte length of the underlying file.
    fn size(&self) -> Result<u64, FsError>;

    /// Write all of `data` at the handle's current write position.
    fn write_all(&mut self, data: &[u8]) -> Result<(), FsError>;
}

/// Byte-oriented persistence injection point: a filesystem backend that hands out
/// [`ClawFile`] handles.
///
/// This is a `std::fs`-like HAL owned as a runtime filesystem instance.
/// Filesystem identity is explicit: stateful implementations such as [`MemFs`]
/// keep their namespace in the instance, and owners that intentionally share
/// one instance do so through an [`Arc`].
///
/// Implementations must provide any synchronization required for concurrent
/// calls through `&self`; callers do not wrap the filesystem in a lock.
/// [`Arc`] is used only when multiple long-lived owners need the same instance.
///
/// Two write disciplines coexist:
/// - [`open_append`](ClawFs::open_append) +
///   [`read_exact_at`](ClawFile::read_exact_at) for append-only journals (e.g.
///   the conversation data `.jsonl`), where each turn appends a record and load
///   reads back only the live records by byte offset.
/// - [`write_atomic`](ClawFs::write_atomic) for whole-file checkpoints that must
///   replace the target tear-free: the small index manifest (`.json`) rewritten
///   on compaction/collapse, and the `.jsonl` itself when a collapse rewrites it
///   to drop dead records. The default implementation writes a temporary sibling
///   then [`rename`](ClawFs::rename)s it over the target.
pub trait ClawFs: Send + Sync + 'static {
    /// The open-file handle this filesystem produces.
    type File: ClawFile;

    /// Open an existing file for reading. Returns [`FsError::NotFound`] when
    /// `path` is absent.
    fn open(&self, path: &str) -> Result<Self::File, FsError>;

    /// Create (or truncate) `path` for writing, creating parent directories as
    /// needed. The returned handle starts empty at offset 0.
    fn create(&self, path: &str) -> Result<Self::File, FsError>;

    /// Open `path` for appending, creating it (and parents) if absent.
    ///
    /// Writes through the returned handle go after whatever is already there, so
    /// prior records are never rewritten. A crash mid-append may leave a torn
    /// trailing record, which readers discard; it must never corrupt earlier
    /// records.
    fn open_append(&self, path: &str) -> Result<Self::File, FsError>;

    /// Rename `from` to `to`, replacing `to` if it exists. Returns
    /// [`FsError::NotFound`] when `from` is absent.
    ///
    /// This is the atomic-replace primitive behind
    /// [`write_atomic`](Self::write_atomic): a crash mid-rename leaves either the
    /// old target or the new one, never a torn mix.
    fn rename(&self, from: &str, to: &str) -> Result<(), FsError>;

    /// Recursively create directory `path`, including any missing parents.
    ///
    /// Idempotent: succeeds if the directory already exists. Backends with no
    /// explicit directory concept (e.g. a flat key→bytes map) treat this as a
    /// no-op — their directories exist implicitly the moment a file is written
    /// beneath them, and [`list_dir`](ClawFs::list_dir) on such a path already
    /// reports an empty listing rather than [`FsError::NotFound`].
    fn create_dir_all(&self, path: &str) -> Result<(), FsError>;

    /// Whether `path` currently exists.
    fn exists(&self, path: &str) -> bool;

    /// Remove a file or empty directory at `path`. Removing a missing path
    /// succeeds (idempotent).
    fn remove(&self, path: &str) -> Result<(), FsError>;

    /// List the immediate entry names within directory `path`.
    ///
    /// Returns only the final path component of each entry (e.g. `"light_switch"`),
    /// not joined paths, and in unspecified order — callers that need ordering
    /// sort themselves. Both files and subdirectories are included. Returns
    /// [`FsError::NotFound`] when `path` does not exist.
    fn list_dir(&self, path: &str) -> Result<Vec<String>, FsError>;

    // ----------------------------------------------------------------------
    // Path-addressed conveniences (default methods over the handle primitives).
    //
    // These cover the one-shot cases where a caller does not need to hold a
    // handle. Implementations may override them where a path-level shortcut is
    // cheaper than open+operate (e.g. `len` via `stat`, `write_atomic` with
    // pretty-printing).
    // ----------------------------------------------------------------------

    /// Read the whole file. Returns [`FsError::NotFound`] when `path` is absent.
    fn read(&self, path: &str) -> Result<Vec<u8>, FsError> {
        self.open(path)?.read_to_end()
    }

    /// Read `len` bytes starting at byte `offset` (a one-shot
    /// [`open`](Self::open) + [`read_exact_at`](ClawFile::read_exact_at)).
    fn read_at(&self, path: &str, offset: u64, len: usize) -> Result<Vec<u8>, FsError> {
        self.open(path)?.read_exact_at(offset, len)
    }

    /// Byte length of `path`. Returns [`FsError::NotFound`] when absent.
    fn len(&self, path: &str) -> Result<u64, FsError> {
        self.open(path)?.size()
    }

    /// Append `data` to the end of `path`, creating it if absent (a one-shot
    /// [`open_append`](Self::open_append) + [`write_all`](ClawFile::write_all)).
    fn append(&self, path: &str, data: &[u8]) -> Result<(), FsError> {
        self.open_append(path)?.write_all(data)
    }

    /// Durably replace `path` with `data`.
    ///
    /// The default writes to a temporary `"{path}.tmp"` sibling and
    /// [`rename`](Self::rename)s it over the target so a crash mid-write never
    /// leaves a half-written file — the file is either the old contents or the
    /// new contents, never a torn mix.
    fn write_atomic(&self, path: &str, data: &[u8]) -> Result<(), FsError> {
        let tmp = format!("{path}.tmp");
        {
            let mut file = self.create(&tmp)?;
            file.write_all(data)?;
        }
        self.rename(&tmp, path)
    }
}
