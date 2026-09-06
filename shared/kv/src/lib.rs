//! One concrete `ekv` database over a Platform-provided NOR region.
//!
//! The crate does not define a storage-backend abstraction. [`Database`] wraps
//! `ekv` directly and preserves its read/write transaction lifecycle. Its
//! The Platform owns native-layout resolution and passes only the selected
//! database region to this crate.

#![no_std]

extern crate alloc;

mod flash;

use alloc::boxed::Box;
use alloc::format;
use alloc::string::String;
use core::fmt::Debug;

use ekv::{
    CommitError, Config, Cursor as EkvCursor, CursorError, Database as EkvDatabase, FormatError,
    MountError, ReadError, ReadTransaction as EkvReadTransaction, WriteError,
    WriteTransaction as EkvWriteTransaction,
};
use embassy_sync_06::blocking_mutex::raw::NoopRawMutex;
use embedded_storage_async::nor_flash::NorFlash;
use zerocopy::{Immutable, IntoBytes, KnownLayout, TryFromBytes};

pub use flash::FlashGeometryError;
use flash::NorFlashAdapter;

/// Required operation alignment of the compiled `ekv` format.
pub const ALIGN: usize = ekv::config::ALIGN;
/// Erased byte expected by the compiled `ekv` format.
pub const ERASE_VALUE: u8 = ekv::config::ERASE_VALUE;
/// Maximum page count supported by the compiled `ekv` format.
pub const MAX_PAGE_COUNT: usize = ekv::config::MAX_PAGE_COUNT;
/// Maximum key capacity supported by the compiled `ekv` format.
pub const MAX_KEY_SIZE: usize = ekv::config::MAX_KEY_SIZE;
/// Maximum value capacity supported by the compiled `ekv` format.
pub const MAX_VALUE_SIZE: usize = ekv::config::MAX_VALUE_SIZE;
/// Page size of the compiled `ekv` format.
pub const PAGE_SIZE: usize = ekv::config::PAGE_SIZE;
/// Maximum partition capacity supported by the compiled `ekv` format.
pub const MAX_CAPACITY: usize = PAGE_SIZE * MAX_PAGE_COUNT;

/// Failure while opening a database partition.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum OpenError {
    /// The selected partition cannot host the compiled `ekv` format.
    #[error(transparent)]
    Geometry(#[from] FlashGeometryError),
    /// Formatting blank storage or mounting existing storage failed.
    #[error(transparent)]
    Database(#[from] Error),
}

/// Stable failure from an `ekv` operation.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// The requested key does not exist.
    #[error("key not found")]
    KeyNotFound,
    /// A key exceeds the configured capacity.
    #[error("key is too long")]
    KeyTooLong,
    /// Stored bytes do not form the requested zerocopy value type.
    #[error("stored bytes do not match the requested value type")]
    InvalidValue,
    /// A value exceeds the configured capacity.
    #[error("value is too long")]
    ValueTooLong,
    /// The database has no free space for the write.
    #[error("database is full")]
    Full,
    /// The database is unformatted or corrupted.
    #[error("database is unformatted or corrupted")]
    Corrupted,
    /// An interrupted operation canceled the active transaction.
    #[error("database transaction was canceled")]
    TransactionCanceled,
    /// Writes in one transaction were not submitted in key order.
    #[error("database transaction keys are not sorted")]
    KeysNotSorted,
    /// The underlying partition operation failed.
    #[error("database partition operation failed: {0}")]
    Flash(String),
}

/// Concrete `ekv` database over one validated writable partition.
pub struct Database<P: NorFlash> {
    inner: EkvDatabase<NorFlashAdapter<P>, NoopRawMutex>,
}

/// Byte-representable value accepted by database writes.
pub trait WriteValue: IntoBytes + Immutable {}

impl<T> WriteValue for T where T: ?Sized + IntoBytes + Immutable {}

/// Fixed-layout value accepted by database reads.
///
/// Deriving [`TryFromBytes`], [`IntoBytes`], [`KnownLayout`], and [`Immutable`]
/// ensures values contain neither pointers nor uninitialized padding and can
/// be validated when read from persistent bytes.
pub trait Value: WriteValue + TryFromBytes + KnownLayout + 'static {}

impl<T> Value for T where T: TryFromBytes + IntoBytes + KnownLayout + Immutable + 'static {}

impl<P> Database<P>
where
    P: NorFlash + 'static,
    P::Error: Debug,
{
    /// Opens `ekv` on a validated writable Board partition.
    ///
    /// Completely erased storage is formatted once. Non-erased storage is
    /// mounted as-is, so corrupt state is reported rather than destroyed.
    ///
    /// # Errors
    ///
    /// Returns an error for incompatible geometry, partition I/O, or corrupt
    /// existing database state.
    pub async fn open(partition: P) -> Result<Self, OpenError> {
        let mut flash = NorFlashAdapter::new(partition)?;
        let erased = flash
            .is_erased()
            .await
            .map_err(|error| Error::Flash(format!("{error:?}")))?;
        let inner = EkvDatabase::new(flash, Config::default());
        if erased {
            inner.format().await.map_err(map_format_error)?;
        } else {
            inner.mount().await.map_err(map_mount_error)?;
        }
        Ok(Self { inner })
    }

    /// Opens an `ekv` read transaction.
    pub async fn read_transaction(&self) -> ReadTransaction<'_, P> {
        ReadTransaction {
            inner: self.inner.read_transaction().await,
        }
    }

    /// Opens an `ekv` write transaction.
    ///
    /// Dropping the returned value without [`WriteTransaction::commit`]
    /// discards every staged write.
    pub async fn write_transaction(&self) -> WriteTransaction<'_, P> {
        WriteTransaction {
            inner: self.inner.write_transaction().await,
        }
    }
}

/// In-progress read transaction retaining `ekv` reader isolation.
///
/// A transaction and any iterator opened from it retain a database reader.
/// Drain or drop them promptly; keeping them across unrelated async work can
/// delay write transaction commits.
pub struct ReadTransaction<'database, P: NorFlash + 'database> {
    inner: EkvReadTransaction<'database, NorFlashAdapter<P>, NoopRawMutex>,
}

impl<P> ReadTransaction<'_, P>
where
    P: NorFlash,
    P::Error: Debug,
{
    /// Reads and validates one typed zerocopy value.
    pub async fn read<T: Value>(&self, key: &str) -> Result<T, Error> {
        let mut bytes = alloc::vec![0; core::mem::size_of::<T>()];
        let length = self
            .inner
            .read(key.as_bytes(), &mut bytes)
            .await
            .map_err(map_read_error)?;
        if length != bytes.len() {
            return Err(Error::InvalidValue);
        }
        T::try_read_from_bytes(&bytes).map_err(|_error| Error::InvalidValue)
    }

    /// Opens a streaming iterator over every live key/value entry.
    pub async fn entries(&self) -> Result<EntryIterator<'_, P>, Error> {
        let inner = self.inner.read_all().await.map_err(map_iteration_error)?;
        Ok(EntryIterator {
            inner,
            key: [0; MAX_KEY_SIZE],
            value: Box::new([0; MAX_VALUE_SIZE]),
        })
    }

    /// Opens a streaming iterator over live entries in `[start, end)`.
    ///
    /// Bounds use the database's bytewise lexical key ordering. They borrow
    /// the transaction for the iterator's lifetime.
    pub async fn entries_in_range<'a>(
        &'a self,
        start: &'a str,
        end: &'a str,
    ) -> Result<EntryIterator<'a, P>, Error> {
        let inner = self
            .inner
            .read_range(start.as_bytes()..end.as_bytes())
            .await
            .map_err(map_iteration_error)?;
        Ok(EntryIterator {
            inner,
            key: [0; MAX_KEY_SIZE],
            value: Box::new([0; MAX_VALUE_SIZE]),
        })
    }
}

/// One borrowed key/value entry returned by [`EntryIterator`].
#[derive(Debug, PartialEq, Eq)]
pub struct EntryRef<'iterator> {
    key: &'iterator str,
    value: &'iterator [u8],
}

impl<'iterator> EntryRef<'iterator> {
    /// Returns the entry's key.
    #[must_use]
    pub fn key(&self) -> &'iterator str {
        self.key
    }

    /// Validates and copies the entry value as a fixed-layout type.
    pub fn value<T: Value>(&self) -> Result<T, Error> {
        decode_value(self.value)
    }

    /// Returns the stored value bytes.
    #[must_use]
    pub fn value_bytes(&self) -> &'iterator [u8] {
        self.value
    }
}

/// Streaming iterator over committed key/value entries.
///
/// Each returned entry borrows this iterator and remains valid until the next
/// call to [`Self::next`]. Drain or drop the iterator promptly because it keeps
/// its read transaction active and can delay writer commits.
pub struct EntryIterator<'database, P: NorFlash + 'database> {
    inner: EkvCursor<'database, NorFlashAdapter<P>, NoopRawMutex>,
    key: [u8; MAX_KEY_SIZE],
    value: Box<[u8; MAX_VALUE_SIZE]>,
}

impl<P> EntryIterator<'_, P>
where
    P: NorFlash,
    P::Error: Debug,
{
    /// Advances to the next live entry.
    pub async fn next(&mut self) -> Result<Option<EntryRef<'_>>, Error> {
        let Some((key_length, value_length)) = self
            .inner
            .next(&mut self.key, self.value.as_mut())
            .await
            .map_err(map_cursor_error)?
        else {
            return Ok(None);
        };
        let key = self.key.get(..key_length).ok_or(Error::Corrupted)?;
        let key = core::str::from_utf8(key).map_err(|_error| Error::Corrupted)?;
        let value = self.value.get(..value_length).ok_or(Error::Corrupted)?;
        Ok(Some(EntryRef { key, value }))
    }
}

fn decode_value<T: Value>(bytes: &[u8]) -> Result<T, Error> {
    if bytes.len() != core::mem::size_of::<T>() {
        return Err(Error::InvalidValue);
    }
    T::try_read_from_bytes(bytes).map_err(|_error| Error::InvalidValue)
}

/// In-progress write transaction retaining `ekv` atomic commit semantics.
pub struct WriteTransaction<'database, P: NorFlash + 'database> {
    inner: EkvWriteTransaction<'database, NorFlashAdapter<P>, NoopRawMutex>,
}

impl<P> WriteTransaction<'_, P>
where
    P: NorFlash,
    P::Error: Debug,
{
    /// Stages one insert or replacement.
    pub async fn write<T: WriteValue + ?Sized>(
        &mut self,
        key: &str,
        value: &T,
    ) -> Result<(), Error> {
        self.inner
            .write(key.as_bytes(), value.as_bytes())
            .await
            .map_err(map_write_error)
    }

    /// Stages deletion of one key. A missing key is ignored.
    pub async fn delete(&mut self, key: &str) -> Result<(), Error> {
        self.inner
            .delete(key.as_bytes())
            .await
            .map_err(map_write_error)
    }

    /// Atomically commits every staged mutation.
    pub async fn commit(self) -> Result<(), Error> {
        self.inner.commit().await.map_err(map_commit_error)
    }
}

fn flash_error(error: impl Debug) -> Error {
    Error::Flash(format!("{error:?}"))
}

fn map_format_error<E: Debug>(error: FormatError<E>) -> Error {
    match error {
        FormatError::Flash(error) => flash_error(error),
    }
}

fn map_mount_error<E: Debug>(error: MountError<E>) -> Error {
    match error {
        MountError::Corrupted => Error::Corrupted,
        MountError::Flash(error) => flash_error(error),
    }
}

fn map_read_error<E: Debug>(error: ReadError<E>) -> Error {
    match error {
        ReadError::KeyNotFound => Error::KeyNotFound,
        ReadError::KeyTooBig => Error::KeyTooLong,
        ReadError::BufferTooSmall => Error::InvalidValue,
        ReadError::Corrupted => Error::Corrupted,
        ReadError::Flash(error) => flash_error(error),
    }
}

fn map_iteration_error<E: Debug>(error: ekv::Error<E>) -> Error {
    match error {
        ekv::Error::Corrupted => Error::Corrupted,
        ekv::Error::Flash(error) => flash_error(error),
    }
}

fn map_cursor_error<E: Debug>(error: CursorError<E>) -> Error {
    match error {
        CursorError::KeyBufferTooSmall | CursorError::ValueBufferTooSmall => Error::Corrupted,
        CursorError::Corrupted => Error::Corrupted,
        CursorError::Flash(error) => flash_error(error),
    }
}

fn map_write_error<E: Debug>(error: WriteError<E>) -> Error {
    match error {
        WriteError::NotSorted => Error::KeysNotSorted,
        WriteError::KeyTooBig => Error::KeyTooLong,
        WriteError::ValueTooBig => Error::ValueTooLong,
        WriteError::TransactionCanceled => Error::TransactionCanceled,
        WriteError::Full => Error::Full,
        WriteError::Corrupted => Error::Corrupted,
        WriteError::Flash(error) => flash_error(error),
    }
}

fn map_commit_error<E: Debug>(error: CommitError<E>) -> Error {
    match error {
        CommitError::TransactionCanceled => Error::TransactionCanceled,
        CommitError::Corrupted => Error::Corrupted,
        CommitError::Flash(error) => flash_error(error),
    }
}
