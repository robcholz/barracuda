//! Plugin namespace policy over the concrete shared key-value database.

use alloc::format;
use alloc::rc::Rc;
use alloc::string::String;
use core::fmt::Debug;

use barracuda_kv::{
    Database, EntryIterator as KvEntryIterator, Error as KvError,
    ReadTransaction as KvReadTransaction, Value, WriteTransaction as KvWriteTransaction,
    WriteValue, MAX_KEY_SIZE,
};
use embedded_storage_async::nor_flash::NorFlash;

use crate::PluginId;

const NAMESPACE_FORMAT: u8 = 1;

/// Result returned by Plugin-scoped storage operations.
pub type StorageResult<T> = Result<T, StorageError>;

/// Failure while using Plugin-scoped storage.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum StorageError {
    /// The caller key does not fit after the Plugin namespace prefix.
    #[error("storage key is too long; maximum for this Plugin is {max} bytes")]
    KeyTooLong {
        /// Maximum caller key length for this Plugin.
        max: usize,
    },
    /// The concrete shared database operation failed.
    #[error(transparent)]
    Database(#[from] KvError),
}

/// Persistent key-value storage isolated to one Plugin identity.
///
/// This is the Plugin-facing storage contract. It deliberately contains no
/// flash, partition, filesystem, or Platform types.
#[allow(async_fn_in_trait)]
pub trait PluginStorage: Clone + 'static {
    /// Read transaction created by this storage implementation.
    type ReadTransaction<'a>: PluginReadTransaction
    where
        Self: 'a;
    /// Write transaction created by this storage implementation.
    type WriteTransaction<'a>: PluginWriteTransaction
    where
        Self: 'a;

    /// Returns the maximum caller key length available in this scope.
    fn max_key_size(&self) -> usize;

    /// Opens an isolated read transaction.
    async fn read_transaction(&self) -> Self::ReadTransaction<'_>;

    /// Opens an isolated write transaction.
    async fn write_transaction(&self) -> Self::WriteTransaction<'_>;

    /// Reads one typed value, returning `None` when the key does not exist.
    async fn get<T: Value>(&self, key: &str) -> StorageResult<Option<T>>;

    /// Inserts or replaces one byte-representable value atomically.
    async fn put<T: WriteValue + ?Sized>(&self, key: &str, value: &T) -> StorageResult<()>;

    /// Deletes one key atomically.
    async fn delete(&self, key: &str) -> StorageResult<()>;
}

/// Plugin-facing read transaction contract.
///
/// This transaction and its iterators retain a shared database reader. Drain
/// or drop them promptly instead of holding them across unrelated async work,
/// because they can delay write transaction commits.
#[allow(async_fn_in_trait)]
pub trait PluginReadTransaction {
    /// Streaming entry iterator created by this transaction.
    type EntryIterator<'a>: PluginEntryIterator
    where
        Self: 'a;
    /// Reads and validates one typed value.
    async fn read<T: Value>(&self, key: &str) -> StorageResult<T>;

    /// Opens a streaming iterator over this Plugin's live entries.
    async fn entries(&self) -> StorageResult<Self::EntryIterator<'_>>;
}

/// One borrowed entry from a Plugin namespace.
#[derive(Debug, PartialEq, Eq)]
pub struct PluginEntry<'iterator> {
    key: &'iterator str,
    value: &'iterator [u8],
}

impl PluginEntry<'_> {
    /// Returns the key relative to the Plugin namespace.
    #[must_use]
    pub fn key(&self) -> &str {
        self.key
    }

    /// Validates and copies the entry value as a fixed-layout type.
    pub fn value<T: Value>(&self) -> StorageResult<T> {
        if self.value.len() != core::mem::size_of::<T>() {
            return Err(StorageError::Database(KvError::InvalidValue));
        }
        T::try_read_from_bytes(self.value)
            .map_err(|_error| StorageError::Database(KvError::InvalidValue))
    }

    /// Returns the stored value bytes.
    #[must_use]
    pub fn value_bytes(&self) -> &[u8] {
        self.value
    }
}

/// Streaming iterator over entries in one Plugin namespace.
///
/// Each returned entry is relative to the Plugin namespace, borrows the
/// iterator, and remains valid until the next call to [`Self::next`]. Drain or
/// drop it promptly because it retains the underlying read transaction.
#[allow(async_fn_in_trait)]
pub trait PluginEntryIterator {
    /// Advances to the next live Plugin entry in lexical key order.
    async fn next(&mut self) -> StorageResult<Option<PluginEntry<'_>>>;
}

/// Plugin-facing atomic write transaction contract.
#[allow(async_fn_in_trait)]
pub trait PluginWriteTransaction: Sized {
    /// Stages one insert or replacement.
    async fn write<T: WriteValue + ?Sized>(&mut self, key: &str, value: &T) -> StorageResult<()>;

    /// Stages deletion of one key.
    async fn delete(&mut self, key: &str) -> StorageResult<()>;

    /// Atomically commits every staged mutation.
    async fn commit(self) -> StorageResult<()>;
}

/// Cloneable typed storage restricted to one Plugin namespace.
pub(crate) struct ScopedStorage<P: NorFlash + 'static> {
    database: Rc<Database<P>>,
    prefix: String,
    prefix_end: String,
}

impl<P: NorFlash + 'static> Clone for ScopedStorage<P> {
    fn clone(&self) -> Self {
        Self {
            database: Rc::clone(&self.database),
            prefix: self.prefix.clone(),
            prefix_end: self.prefix_end.clone(),
        }
    }
}

impl<P> ScopedStorage<P>
where
    P: NorFlash + 'static,
    P::Error: Debug,
{
    pub(crate) fn new(database: Rc<Database<P>>, plugin_id: &PluginId) -> Self {
        let namespace = plugin_id.as_str().as_bytes();
        let prefix = format!(
            "{NAMESPACE_FORMAT}:{}:{}:",
            namespace.len(),
            plugin_id.as_str()
        );
        let prefix_end = format!(
            "{NAMESPACE_FORMAT}:{}:{};",
            namespace.len(),
            plugin_id.as_str()
        );
        Self {
            database,
            prefix,
            prefix_end,
        }
    }

    /// Returns the maximum caller key length available in this scope.
    #[must_use]
    fn max_key_size(&self) -> usize {
        MAX_KEY_SIZE.saturating_sub(self.prefix.len())
    }

    /// Opens a read transaction restricted to this Plugin namespace.
    async fn read_transaction(&self) -> ScopedReadTransaction<'_, P> {
        ScopedReadTransaction {
            inner: self.database.read_transaction().await,
            prefix: self.prefix.as_str(),
            prefix_end: self.prefix_end.as_str(),
        }
    }

    /// Opens a write transaction restricted to this Plugin namespace.
    ///
    /// Keys retain `ekv`'s ordering requirement. Dropping the transaction
    /// without [`ScopedWriteTransaction::commit`] rolls back every staged write.
    async fn write_transaction(&self) -> ScopedWriteTransaction<'_, P> {
        ScopedWriteTransaction {
            inner: self.database.write_transaction().await,
            prefix: self.prefix.as_str(),
        }
    }

    /// Reads one typed zerocopy value from this Plugin's scope.
    async fn get<T: Value>(&self, key: &str) -> StorageResult<Option<T>> {
        let transaction = self.read_transaction().await;
        match transaction.read(key).await {
            Ok(value) => Ok(Some(value)),
            Err(StorageError::Database(KvError::KeyNotFound)) => Ok(None),
            Err(error) => Err(error),
        }
    }

    /// Inserts or replaces one byte-representable value in a committed transaction.
    async fn put<T: WriteValue + ?Sized>(&self, key: &str, value: &T) -> StorageResult<()> {
        let mut transaction = self.write_transaction().await;
        transaction.write(key, value).await?;
        transaction.commit().await
    }

    /// Deletes one key in a committed write transaction.
    async fn delete(&self, key: &str) -> StorageResult<()> {
        let mut transaction = self.write_transaction().await;
        transaction.delete(key).await?;
        transaction.commit().await
    }
}

impl<P> PluginStorage for ScopedStorage<P>
where
    P: NorFlash + 'static,
    P::Error: Debug,
{
    type ReadTransaction<'a> = ScopedReadTransaction<'a, P>;
    type WriteTransaction<'a> = ScopedWriteTransaction<'a, P>;

    fn max_key_size(&self) -> usize {
        ScopedStorage::max_key_size(self)
    }

    async fn read_transaction(&self) -> Self::ReadTransaction<'_> {
        ScopedStorage::read_transaction(self).await
    }

    async fn write_transaction(&self) -> Self::WriteTransaction<'_> {
        ScopedStorage::write_transaction(self).await
    }

    async fn get<T: Value>(&self, key: &str) -> StorageResult<Option<T>> {
        ScopedStorage::get(self, key).await
    }

    async fn put<T: WriteValue + ?Sized>(&self, key: &str, value: &T) -> StorageResult<()> {
        ScopedStorage::put(self, key, value).await
    }

    async fn delete(&self, key: &str) -> StorageResult<()> {
        ScopedStorage::delete(self, key).await
    }
}

/// In-progress `ekv` read transaction restricted to one Plugin namespace.
pub(crate) struct ScopedReadTransaction<'database, P: NorFlash + 'database> {
    inner: KvReadTransaction<'database, P>,
    prefix: &'database str,
    prefix_end: &'database str,
}

impl<P> ScopedReadTransaction<'_, P>
where
    P: NorFlash,
    P::Error: Debug,
{
    /// Reads and validates one scoped zerocopy value.
    pub async fn read<T: Value>(&self, key: &str) -> StorageResult<T> {
        let key = scoped_key(self.prefix, key)?;
        self.inner.read(&key).await.map_err(StorageError::from)
    }

    /// Opens a streaming entry iterator restricted to this Plugin namespace.
    pub async fn entries(&self) -> StorageResult<ScopedEntryIterator<'_, P>> {
        Ok(ScopedEntryIterator {
            inner: self
                .inner
                .entries_in_range(self.prefix, self.prefix_end)
                .await?,
            prefix_length: self.prefix.len(),
            key: [0; MAX_KEY_SIZE],
            key_length: 0,
        })
    }
}

impl<P> PluginReadTransaction for ScopedReadTransaction<'_, P>
where
    P: NorFlash,
    P::Error: Debug,
{
    type EntryIterator<'a>
        = ScopedEntryIterator<'a, P>
    where
        Self: 'a;
    async fn read<T: Value>(&self, key: &str) -> StorageResult<T> {
        ScopedReadTransaction::read(self, key).await
    }

    async fn entries(&self) -> StorageResult<Self::EntryIterator<'_>> {
        ScopedReadTransaction::entries(self).await
    }
}

/// Iterator that removes the physical namespace from every returned entry.
pub(crate) struct ScopedEntryIterator<'database, P: NorFlash + 'database> {
    inner: KvEntryIterator<'database, P>,
    prefix_length: usize,
    key: [u8; MAX_KEY_SIZE],
    key_length: usize,
}

impl<P> PluginEntryIterator for ScopedEntryIterator<'_, P>
where
    P: NorFlash,
    P::Error: Debug,
{
    async fn next(&mut self) -> StorageResult<Option<PluginEntry<'_>>> {
        let Some(entry) = self.inner.next().await? else {
            return Ok(None);
        };
        let relative = entry
            .key()
            .get(self.prefix_length..)
            .ok_or(StorageError::Database(KvError::Corrupted))?;
        let destination = self
            .key
            .get_mut(..relative.len())
            .ok_or(StorageError::Database(KvError::Corrupted))?;
        destination.copy_from_slice(relative.as_bytes());
        self.key_length = relative.len();
        let key = self
            .key
            .get(..self.key_length)
            .ok_or(StorageError::Database(KvError::Corrupted))?;
        let key = core::str::from_utf8(key)
            .map_err(|_error| StorageError::Database(KvError::Corrupted))?;
        Ok(Some(PluginEntry {
            key,
            value: entry.value_bytes(),
        }))
    }
}

/// In-progress `ekv` write transaction restricted to one Plugin namespace.
pub(crate) struct ScopedWriteTransaction<'database, P: NorFlash + 'database> {
    inner: KvWriteTransaction<'database, P>,
    prefix: &'database str,
}

impl<P> ScopedWriteTransaction<'_, P>
where
    P: NorFlash,
    P::Error: Debug,
{
    /// Stages one scoped insert or replacement.
    pub async fn write<T: WriteValue + ?Sized>(
        &mut self,
        key: &str,
        value: &T,
    ) -> StorageResult<()> {
        let key = scoped_key(self.prefix, key)?;
        self.inner
            .write(&key, value)
            .await
            .map_err(StorageError::from)
    }

    /// Stages deletion of one scoped key.
    pub async fn delete(&mut self, key: &str) -> StorageResult<()> {
        let key = scoped_key(self.prefix, key)?;
        self.inner.delete(&key).await.map_err(StorageError::from)
    }

    /// Atomically commits every staged mutation.
    pub async fn commit(self) -> StorageResult<()> {
        self.inner.commit().await.map_err(StorageError::from)
    }
}

impl<P> PluginWriteTransaction for ScopedWriteTransaction<'_, P>
where
    P: NorFlash,
    P::Error: Debug,
{
    async fn write<T: WriteValue + ?Sized>(&mut self, key: &str, value: &T) -> StorageResult<()> {
        ScopedWriteTransaction::write(self, key, value).await
    }

    async fn delete(&mut self, key: &str) -> StorageResult<()> {
        ScopedWriteTransaction::delete(self, key).await
    }

    async fn commit(self) -> StorageResult<()> {
        ScopedWriteTransaction::commit(self).await
    }
}

fn scoped_key(prefix: &str, key: &str) -> StorageResult<String> {
    if prefix.len().saturating_add(key.len()) > MAX_KEY_SIZE {
        return Err(StorageError::KeyTooLong {
            max: MAX_KEY_SIZE.saturating_sub(prefix.len()),
        });
    }
    let mut scoped = String::with_capacity(prefix.len().saturating_add(key.len()));
    scoped.push_str(prefix);
    scoped.push_str(key);
    Ok(scoped)
}
