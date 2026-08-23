//! Raw byte storage backed by `ekv` and scoped to one Plugin namespace.

use alloc::boxed::Box;
use alloc::format;
use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;
use core::fmt::Debug;
use core::future::Future;
use core::pin::Pin;

use ekv::flash::Flash;
use ekv::{CommitError, Config, Database, FormatError, MountError, ReadError, WriteError};
use embassy_sync_06::blocking_mutex::raw::RawMutex;

use crate::PluginId;

const NAMESPACE_FORMAT: u8 = 1;
const NAMESPACE_HEADER_BYTES: usize = 2;

/// Result returned by scoped storage operations.
pub type StorageResult<T> = Result<T, StorageError>;

/// Failure while using Plugin storage.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum StorageError {
    /// The caller key does not fit after the Plugin namespace prefix.
    #[error("storage key is too long; maximum for this Plugin is {max} bytes")]
    KeyTooLong {
        /// Maximum caller key length for this Plugin.
        max: usize,
    },
    /// The value exceeds the configured `ekv` value capacity.
    #[error("storage value is too long; maximum is {max} bytes")]
    ValueTooLong {
        /// Maximum value length configured for `ekv`.
        max: usize,
    },
    /// One atomic batch contains the same key more than once.
    #[error("an atomic storage batch contains a duplicate key")]
    DuplicateKey,
    /// The database has no free space for the write.
    #[error("Plugin storage is full")]
    Full,
    /// The database is unformatted or corrupted.
    #[error("Plugin storage is unformatted or corrupted")]
    Corrupted,
    /// An interrupted operation canceled the active transaction.
    #[error("Plugin storage transaction was canceled")]
    TransactionCanceled,
    /// The prepared transaction violated an `ekv` ordering invariant.
    #[error("Plugin storage transaction has an invalid key order")]
    InvalidTransaction,
    /// The underlying flash operation failed.
    #[error("Plugin storage flash operation failed: {0}")]
    Flash(String),
}

/// One mutation in an atomic scoped storage commit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StorageMutation<'a> {
    /// Insert or replace one raw byte value.
    Put {
        /// Caller-controlled key within the Plugin scope.
        key: &'a [u8],
        /// Raw value stored for `key`.
        value: &'a [u8],
    },
    /// Delete one key. Missing keys are ignored.
    Delete {
        /// Caller-controlled key within the Plugin scope.
        key: &'a [u8],
    },
}

impl StorageMutation<'_> {
    fn key(&self) -> &[u8] {
        match self {
            Self::Put { key, .. } | Self::Delete { key } => key,
        }
    }
}

pub(crate) enum PreparedMutation {
    Put { key: Vec<u8>, value: Vec<u8> },
    Delete { key: Vec<u8> },
}

impl PreparedMutation {
    fn key(&self) -> &[u8] {
        match self {
            Self::Put { key, .. } | Self::Delete { key } => key,
        }
    }
}

type StorageFuture<'a, T> = Pin<Box<dyn Future<Output = T> + 'a>>;

pub(crate) trait StorageBackend {
    fn get<'a>(&'a self, key: &'a [u8]) -> StorageFuture<'a, StorageResult<Option<Vec<u8>>>>;

    fn commit<'a>(
        &'a self,
        mutations: &'a [PreparedMutation],
    ) -> StorageFuture<'a, StorageResult<()>>;
}

/// An `ekv` database owned by the Plugin framework.
///
/// Applications select the raw flash implementation and Embassy raw mutex.
/// Plugins never receive this value directly; [`PluginManager`](crate::PluginManager)
/// turns it into namespace-restricted [`ScopedStorage`] capabilities.
pub struct EkvStore<F: Flash, M: RawMutex> {
    database: Database<F, M>,
}

impl<F: Flash, M: RawMutex> EkvStore<F, M> {
    /// Creates a store over raw flash without reading or formatting it.
    #[must_use]
    pub fn new(flash: F, config: Config) -> Self {
        Self {
            database: Database::new(flash, config),
        }
    }

    /// Formats the entire database as empty storage.
    ///
    /// # Errors
    ///
    /// Returns an error when the underlying flash erase or write fails.
    pub async fn format(&self) -> StorageResult<()> {
        self.database.format().await.map_err(map_format_error)
    }

    /// Eagerly mounts an existing database.
    ///
    /// `ekv` also mounts lazily on first access; this method lets the
    /// application distinguish initialization from normal Plugin operations.
    ///
    /// # Errors
    ///
    /// Returns an error when storage is unformatted, corrupted, or the flash
    /// cannot be read.
    pub async fn mount(&self) -> StorageResult<()> {
        self.database.mount().await.map_err(map_mount_error)
    }
}

impl<F, M> StorageBackend for EkvStore<F, M>
where
    F: Flash + 'static,
    M: RawMutex + 'static,
{
    fn get<'a>(&'a self, key: &'a [u8]) -> StorageFuture<'a, StorageResult<Option<Vec<u8>>>> {
        Box::pin(async move {
            let transaction = self.database.read_transaction().await;
            let mut value = vec![0; ekv::config::MAX_VALUE_SIZE];
            match transaction.read(key, &mut value).await {
                Ok(length) => {
                    value.truncate(length);
                    Ok(Some(value))
                }
                Err(ReadError::KeyNotFound) => Ok(None),
                Err(error) => Err(map_read_error(error)),
            }
        })
    }

    fn commit<'a>(
        &'a self,
        mutations: &'a [PreparedMutation],
    ) -> StorageFuture<'a, StorageResult<()>> {
        Box::pin(async move {
            let mut transaction = self.database.write_transaction().await;
            for mutation in mutations {
                match mutation {
                    PreparedMutation::Put { key, value } => {
                        transaction
                            .write(key, value)
                            .await
                            .map_err(map_write_error)?;
                    }
                    PreparedMutation::Delete { key } => {
                        transaction.delete(key).await.map_err(map_write_error)?;
                    }
                }
            }
            transaction.commit().await.map_err(map_commit_error)
        })
    }
}

/// Cloneable raw byte storage restricted to one Plugin namespace.
///
/// The namespace prefix is applied internally and cannot be supplied or
/// modified by Plugin code. Clones retain exactly the same scope, allowing a
/// Plugin to share its storage with any of its Components.
#[derive(Clone)]
pub struct ScopedStorage {
    backend: Rc<dyn StorageBackend>,
    prefix: Vec<u8>,
}

impl ScopedStorage {
    pub(crate) fn new(backend: Rc<dyn StorageBackend>, plugin_id: &PluginId) -> Self {
        let mut prefix =
            Vec::with_capacity(NAMESPACE_HEADER_BYTES.saturating_add(plugin_id.as_str().len()));
        prefix.push(NAMESPACE_FORMAT);
        prefix.push(u8::try_from(plugin_id.as_str().len()).unwrap_or(0));
        prefix.extend_from_slice(plugin_id.as_str().as_bytes());
        Self { backend, prefix }
    }

    /// Returns the maximum caller key length available in this scope.
    #[must_use]
    pub fn max_key_size(&self) -> usize {
        ekv::config::MAX_KEY_SIZE.saturating_sub(self.prefix.len())
    }

    /// Reads one raw value from this Plugin's scope.
    ///
    /// # Errors
    ///
    /// Returns an error for an oversized key or database failure.
    pub async fn get(&self, key: &[u8]) -> StorageResult<Option<Vec<u8>>> {
        let key = self.scoped_key(key)?;
        self.backend.get(&key).await
    }

    /// Atomically inserts or replaces one raw value.
    ///
    /// # Errors
    ///
    /// Returns an error for an oversized key or value, or database failure.
    pub async fn put(&self, key: &[u8], value: &[u8]) -> StorageResult<()> {
        self.commit(core::slice::from_ref(&StorageMutation::Put { key, value }))
            .await
    }

    /// Atomically deletes one key. Missing keys are ignored.
    ///
    /// # Errors
    ///
    /// Returns an error for an oversized key or database failure.
    pub async fn delete(&self, key: &[u8]) -> StorageResult<()> {
        self.commit(core::slice::from_ref(&StorageMutation::Delete { key }))
            .await
    }

    /// Atomically commits multiple raw mutations within this Plugin's scope.
    ///
    /// Caller ordering is irrelevant. The wrapper sorts namespaced keys for
    /// `ekv` and rejects duplicate caller keys before opening a transaction.
    ///
    /// # Errors
    ///
    /// Returns an error for duplicate or oversized data, or database failure.
    pub async fn commit(&self, mutations: &[StorageMutation<'_>]) -> StorageResult<()> {
        let mut prepared = Vec::with_capacity(mutations.len());
        for mutation in mutations {
            let key = self.scoped_key(mutation.key())?;
            match mutation {
                StorageMutation::Put { value, .. } => {
                    if value.len() > ekv::config::MAX_VALUE_SIZE {
                        return Err(StorageError::ValueTooLong {
                            max: ekv::config::MAX_VALUE_SIZE,
                        });
                    }
                    prepared.push(PreparedMutation::Put {
                        key,
                        value: value.to_vec(),
                    });
                }
                StorageMutation::Delete { .. } => {
                    prepared.push(PreparedMutation::Delete { key });
                }
            }
        }

        prepared.sort_by(|left, right| left.key().cmp(right.key()));
        reject_duplicate_keys(&prepared)?;
        self.backend.commit(&prepared).await
    }

    fn scoped_key(&self, key: &[u8]) -> StorageResult<Vec<u8>> {
        if key.len() > self.max_key_size() {
            return Err(StorageError::KeyTooLong {
                max: self.max_key_size(),
            });
        }
        let mut scoped = Vec::with_capacity(self.prefix.len().saturating_add(key.len()));
        scoped.extend_from_slice(&self.prefix);
        scoped.extend_from_slice(key);
        Ok(scoped)
    }
}

fn reject_duplicate_keys(mutations: &[PreparedMutation]) -> StorageResult<()> {
    let mut previous: Option<&[u8]> = None;
    for mutation in mutations {
        let key = mutation.key();
        if previous == Some(key) {
            return Err(StorageError::DuplicateKey);
        }
        previous = Some(key);
    }
    Ok(())
}

fn flash_error(error: impl Debug) -> StorageError {
    StorageError::Flash(format!("{error:?}"))
}

fn map_format_error<E: Debug>(error: FormatError<E>) -> StorageError {
    match error {
        FormatError::Flash(error) => flash_error(error),
    }
}

fn map_mount_error<E: Debug>(error: MountError<E>) -> StorageError {
    match error {
        MountError::Corrupted => StorageError::Corrupted,
        MountError::Flash(error) => flash_error(error),
    }
}

fn map_read_error<E: Debug>(error: ReadError<E>) -> StorageError {
    match error {
        ReadError::KeyNotFound => StorageError::Corrupted,
        ReadError::KeyTooBig => StorageError::KeyTooLong {
            max: ekv::config::MAX_KEY_SIZE,
        },
        ReadError::BufferTooSmall => StorageError::ValueTooLong {
            max: ekv::config::MAX_VALUE_SIZE,
        },
        ReadError::Corrupted => StorageError::Corrupted,
        ReadError::Flash(error) => flash_error(error),
    }
}

fn map_write_error<E: Debug>(error: WriteError<E>) -> StorageError {
    match error {
        WriteError::NotSorted => StorageError::InvalidTransaction,
        WriteError::KeyTooBig => StorageError::KeyTooLong {
            max: ekv::config::MAX_KEY_SIZE,
        },
        WriteError::ValueTooBig => StorageError::ValueTooLong {
            max: ekv::config::MAX_VALUE_SIZE,
        },
        WriteError::TransactionCanceled => StorageError::TransactionCanceled,
        WriteError::Full => StorageError::Full,
        WriteError::Corrupted => StorageError::Corrupted,
        WriteError::Flash(error) => flash_error(error),
    }
}

fn map_commit_error<E: Debug>(error: CommitError<E>) -> StorageError {
    match error {
        CommitError::TransactionCanceled => StorageError::TransactionCanceled,
        CommitError::Corrupted => StorageError::Corrupted,
        CommitError::Flash(error) => flash_error(error),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use alloc::rc::Rc;
    use alloc::vec;

    use ekv::flash::MemFlash;
    use futures_lite::future::block_on;

    use ekv::{CommitError, FormatError, MountError, ReadError, WriteError};

    use super::{
        map_commit_error, map_format_error, map_mount_error, map_read_error, map_write_error,
        EkvStore, ScopedStorage, StorageBackend, StorageError, StorageMutation,
    };
    use crate::{NoopRawMutex, PluginId};

    fn storage() -> ScopedStorage {
        let store = Rc::new(EkvStore::<MemFlash, NoopRawMutex>::new(
            MemFlash::new(),
            ekv::Config::default(),
        ));
        block_on(store.format()).expect("format test store");
        block_on(store.mount()).expect("mount test store");
        let backend: Rc<dyn StorageBackend> = store;
        ScopedStorage::new(
            backend,
            &PluginId::try_from("test").expect("valid Plugin ID"),
        )
    }

    #[test]
    fn caller_key_capacity_accounts_for_namespace() {
        let storage = storage();
        let boundary_key = vec![0; storage.max_key_size()];
        let oversized_key = vec![0; storage.max_key_size().saturating_add(1)];

        block_on(storage.put(&boundary_key, b"value")).expect("write boundary key");
        let error = block_on(storage.get(&oversized_key)).expect_err("reject oversized key");

        assert_eq!(
            error,
            StorageError::KeyTooLong {
                max: storage.max_key_size()
            }
        );
    }

    #[test]
    fn atomic_batch_rejects_duplicate_keys() {
        let storage = storage();

        let error = block_on(storage.commit(&[
            StorageMutation::Put {
                key: b"same",
                value: b"first",
            },
            StorageMutation::Delete { key: b"same" },
        ]))
        .expect_err("reject duplicate key");

        assert_eq!(error, StorageError::DuplicateKey);
    }

    #[test]
    fn oversized_value_is_rejected_before_writing() {
        let storage = storage();
        let value = vec![0; ekv::config::MAX_VALUE_SIZE.saturating_add(1)];

        let error = block_on(storage.put(b"large", &value)).expect_err("reject large value");

        assert_eq!(
            error,
            StorageError::ValueTooLong {
                max: ekv::config::MAX_VALUE_SIZE
            }
        );
    }

    #[test]
    fn delete_removes_only_the_selected_key() {
        let storage = storage();
        block_on(storage.put(b"remove", b"gone")).expect("write removed key");
        block_on(storage.put(b"keep", b"present")).expect("write retained key");

        block_on(storage.delete(b"remove")).expect("delete key");

        assert_eq!(block_on(storage.get(b"remove")).expect("read key"), None);
        assert_eq!(
            block_on(storage.get(b"keep")).expect("read retained key"),
            Some(b"present".to_vec())
        );
    }

    #[test]
    fn ekv_errors_map_to_stable_storage_errors() {
        assert_eq!(
            map_format_error(FormatError::Flash(7_u8)),
            StorageError::Flash("7".into())
        );
        assert_eq!(
            map_mount_error::<u8>(MountError::Corrupted),
            StorageError::Corrupted
        );
        assert_eq!(
            map_mount_error(MountError::Flash(7_u8)),
            StorageError::Flash("7".into())
        );

        assert_eq!(
            map_read_error::<u8>(ReadError::KeyNotFound),
            StorageError::Corrupted
        );
        assert_eq!(
            map_read_error::<u8>(ReadError::KeyTooBig),
            StorageError::KeyTooLong {
                max: ekv::config::MAX_KEY_SIZE
            }
        );
        assert_eq!(
            map_read_error::<u8>(ReadError::BufferTooSmall),
            StorageError::ValueTooLong {
                max: ekv::config::MAX_VALUE_SIZE
            }
        );
        assert_eq!(
            map_read_error::<u8>(ReadError::Corrupted),
            StorageError::Corrupted
        );
        assert_eq!(
            map_read_error(ReadError::Flash(7_u8)),
            StorageError::Flash("7".into())
        );

        assert_eq!(
            map_write_error::<u8>(WriteError::NotSorted),
            StorageError::InvalidTransaction
        );
        assert_eq!(
            map_write_error::<u8>(WriteError::KeyTooBig),
            StorageError::KeyTooLong {
                max: ekv::config::MAX_KEY_SIZE
            }
        );
        assert_eq!(
            map_write_error::<u8>(WriteError::ValueTooBig),
            StorageError::ValueTooLong {
                max: ekv::config::MAX_VALUE_SIZE
            }
        );
        assert_eq!(
            map_write_error::<u8>(WriteError::TransactionCanceled),
            StorageError::TransactionCanceled
        );
        assert_eq!(map_write_error::<u8>(WriteError::Full), StorageError::Full);
        assert_eq!(
            map_write_error::<u8>(WriteError::Corrupted),
            StorageError::Corrupted
        );
        assert_eq!(
            map_write_error(WriteError::Flash(7_u8)),
            StorageError::Flash("7".into())
        );

        assert_eq!(
            map_commit_error::<u8>(CommitError::TransactionCanceled),
            StorageError::TransactionCanceled
        );
        assert_eq!(
            map_commit_error::<u8>(CommitError::Corrupted),
            StorageError::Corrupted
        );
        assert_eq!(
            map_commit_error(CommitError::Flash(7_u8)),
            StorageError::Flash("7".into())
        );
    }
}
