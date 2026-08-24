#![no_std]

//! LittleFS backend for `barracuda-vfs`.

extern crate alloc;

use alloc::boxed::Box;
use alloc::string::{String, ToString};
use alloc::sync::Arc;
use alloc::vec;
use alloc::vec::Vec;
use core::marker::PhantomData;

use barracuda_vfs::{
    Backend, BackendFile, BackendFuture, DirEntry, FileType, FsError, Metadata, OpenOptions,
    SeekFrom, VfsBackend,
};
use embedded_storage::nor_flash::{NorFlash, ReadNorFlash};
use generic_array::ArrayLength;
use littlefs2::driver::Storage;
use littlefs2::fs::Filesystem;
use littlefs2::path::PathBuf;
use spin::Mutex;

/// Adapts an owned synchronous NOR-flash partition to `littlefs2::Storage`.
pub struct PartitionStorage<Flash, CacheSize, LookaheadSize, const BLOCK_COUNT: usize> {
    flash: Flash,
    marker: PhantomData<(CacheSize, LookaheadSize)>,
}

impl<Flash, CacheSize, LookaheadSize, const BLOCK_COUNT: usize>
    PartitionStorage<Flash, CacheSize, LookaheadSize, BLOCK_COUNT>
where
    Flash: NorFlash,
{
    /// Validates the partition capacity and creates a LittleFS storage adapter.
    pub fn new(flash: Flash) -> Result<Self, FsError> {
        let required = Flash::ERASE_SIZE
            .checked_mul(BLOCK_COUNT)
            .ok_or(FsError::InvalidInput)?;
        if BLOCK_COUNT == 0 || flash.capacity() < required {
            return Err(FsError::InvalidInput);
        }
        Ok(Self {
            flash,
            marker: PhantomData,
        })
    }

    /// Recovers the owned partition.
    pub fn into_inner(self) -> Flash {
        self.flash
    }
}

impl<Flash, CacheSize, LookaheadSize, const BLOCK_COUNT: usize> Storage
    for PartitionStorage<Flash, CacheSize, LookaheadSize, BLOCK_COUNT>
where
    Flash: NorFlash,
    CacheSize: ArrayLength<u8>,
    LookaheadSize: ArrayLength<u64>,
{
    const READ_SIZE: usize = Flash::READ_SIZE;
    const WRITE_SIZE: usize = Flash::WRITE_SIZE;
    const BLOCK_SIZE: usize = Flash::ERASE_SIZE;
    const BLOCK_COUNT: usize = BLOCK_COUNT;

    type CACHE_SIZE = CacheSize;
    type LOOKAHEAD_SIZE = LookaheadSize;

    fn read(&mut self, offset: usize, buffer: &mut [u8]) -> littlefs2::io::Result<usize> {
        let offset = u32::try_from(offset).map_err(|_| littlefs2::io::Error::IO)?;
        ReadNorFlash::read(&mut self.flash, offset, buffer)
            .map_err(|_| littlefs2::io::Error::IO)?;
        Ok(buffer.len())
    }

    fn write(&mut self, offset: usize, bytes: &[u8]) -> littlefs2::io::Result<usize> {
        let offset = u32::try_from(offset).map_err(|_| littlefs2::io::Error::IO)?;
        NorFlash::write(&mut self.flash, offset, bytes).map_err(|_| littlefs2::io::Error::IO)?;
        Ok(bytes.len())
    }

    fn erase(&mut self, offset: usize, len: usize) -> littlefs2::io::Result<usize> {
        let end = offset.checked_add(len).ok_or(littlefs2::io::Error::IO)?;
        let from = u32::try_from(offset).map_err(|_| littlefs2::io::Error::IO)?;
        let to = u32::try_from(end).map_err(|_| littlefs2::io::Error::IO)?;
        NorFlash::erase(&mut self.flash, from, to).map_err(|_| littlefs2::io::Error::IO)?;
        Ok(len)
    }
}

/// A LittleFS backend over an owned synchronous `littlefs2::Storage`.
///
/// Its VFS methods are asynchronous for API uniformity, but each LittleFS
/// operation completes synchronously when its future is polled.
#[derive(Clone)]
pub struct LittleFs<S: Storage + Send + 'static> {
    storage: Arc<Mutex<S>>,
}

impl<S: Storage + Send + 'static> LittleFs<S> {
    /// Formats `storage` and returns a mounted backend.
    pub fn format(mut storage: S) -> Result<Self, FsError> {
        Filesystem::format(&mut storage).map_err(map_error)?;
        Self::mount(storage)
    }

    /// Validates and opens an already-formatted LittleFS storage device.
    pub fn mount(mut storage: S) -> Result<Self, FsError> {
        Filesystem::mount_and_then(&mut storage, |_| Ok(())).map_err(map_error)?;
        Ok(Self {
            storage: Arc::new(Mutex::new(storage)),
        })
    }

    /// Formats storage only when it cannot already be mounted.
    pub fn mount_or_format(mut storage: S) -> Result<Self, FsError> {
        if !Filesystem::is_mountable(&mut storage) {
            Filesystem::format(&mut storage).map_err(map_error)?;
        }
        Self::mount(storage)
    }

    /// Erases the concrete backend type for mounting.
    pub fn into_backend(self) -> Backend {
        Backend::new(self)
    }

    /// Recovers storage when this is its only remaining owner.
    pub fn into_storage(self) -> Result<S, FsError> {
        Arc::try_unwrap(self.storage)
            .map(Mutex::into_inner)
            .map_err(|_| FsError::Busy)
    }

    /// Reads a whole backend-relative file.
    pub fn read_file(&self, path: &str) -> Result<Vec<u8>, FsError> {
        let path = little_path(path)?;
        self.with_fs(|filesystem| {
            let size = filesystem.metadata(&path)?.len();
            let mut bytes = vec![0; size];
            filesystem.open_file_and_then(&path, |file| {
                let mut filled = 0;
                while filled < bytes.len() {
                    let read = file.read(&mut bytes[filled..])?;
                    if read == 0 {
                        break;
                    }
                    filled += read;
                }
                bytes.truncate(filled);
                Ok(())
            })?;
            Ok(bytes)
        })
        .map_err(map_error)
    }

    /// Replaces a whole backend-relative file.
    pub fn write_file(&self, path: &str, bytes: &[u8]) -> Result<(), FsError> {
        let path = little_path(path)?;
        self.with_fs(|filesystem| filesystem.write(&path, bytes))
            .map_err(map_error)
    }

    fn with_fs<R>(
        &self,
        operation: impl FnOnce(&Filesystem<'_, S>) -> littlefs2::io::Result<R>,
    ) -> littlefs2::io::Result<R> {
        let mut storage = self.storage.lock();
        Filesystem::mount_and_then(&mut *storage, operation)
    }
}

impl<S: Storage + Send + 'static> VfsBackend for LittleFs<S> {
    fn open<'a>(
        &'a self,
        path: &'a str,
        options: &'a OpenOptions,
    ) -> BackendFuture<'a, Result<Box<dyn BackendFile>, FsError>> {
        Box::pin(async move {
            let little_path = little_path(path)?;
            self.with_fs(|filesystem| {
                filesystem.open_file_with_options_and_then(
                    |value| configure(value, options),
                    &little_path,
                    |_| Ok(()),
                )
            })
            .map_err(map_error)?;
            let cursor = if options.is_append() {
                self.metadata(path).await?.len()
            } else {
                0
            };
            let mut runtime_options = OpenOptions::new();
            runtime_options
                .read(options.can_read())
                .write(options.can_write())
                .append(options.is_append());
            Ok(Box::new(LittleFile {
                storage: self.storage.clone(),
                path: path.to_string(),
                cursor,
                options: runtime_options,
            }) as Box<dyn BackendFile>)
        })
    }

    fn metadata<'a>(&'a self, path: &'a str) -> BackendFuture<'a, Result<Metadata, FsError>> {
        Box::pin(async move {
            if path == "/" {
                return Ok(Metadata::new(FileType::Directory, 0));
            }
            let path = little_path(path)?;
            self.with_fs(|filesystem| filesystem.metadata(&path))
                .map(|metadata| {
                    let file_type = if metadata.is_file() {
                        FileType::File
                    } else {
                        FileType::Directory
                    };
                    Metadata::new(file_type, metadata.len() as u64)
                })
                .map_err(map_error)
        })
    }

    fn read_dir<'a>(&'a self, path: &'a str) -> BackendFuture<'a, Result<Vec<DirEntry>, FsError>> {
        Box::pin(async move {
            let path = little_path(path)?;
            self.with_fs(|filesystem| {
                let mut entries = Vec::new();
                filesystem.read_dir_and_then(&path, |directory| {
                    for entry in directory {
                        let entry = entry?;
                        let name = entry.file_name().as_str();
                        if name == "." || name == ".." {
                            continue;
                        }
                        let metadata = entry.metadata();
                        let file_type = if metadata.is_file() {
                            FileType::File
                        } else {
                            FileType::Directory
                        };
                        entries.push(DirEntry::new(
                            name,
                            Metadata::new(file_type, metadata.len() as u64),
                        ));
                    }
                    Ok(())
                })?;
                Ok(entries)
            })
            .map_err(map_error)
        })
    }

    fn create_dir_all<'a>(&'a self, path: &'a str) -> BackendFuture<'a, Result<(), FsError>> {
        Box::pin(async move {
            let path = little_path(path)?;
            self.with_fs(|filesystem| filesystem.create_dir_all(&path))
                .map_err(map_error)
        })
    }

    fn remove_file<'a>(&'a self, path: &'a str) -> BackendFuture<'a, Result<(), FsError>> {
        Box::pin(async move {
            if self.metadata(path).await?.is_dir() {
                return Err(FsError::IsDirectory);
            }
            let path = little_path(path)?;
            self.with_fs(|filesystem| filesystem.remove(&path))
                .map_err(map_error)
        })
    }

    fn remove_dir<'a>(&'a self, path: &'a str) -> BackendFuture<'a, Result<(), FsError>> {
        Box::pin(async move {
            if !self.metadata(path).await?.is_dir() {
                return Err(FsError::NotDirectory);
            }
            let path = little_path(path)?;
            self.with_fs(|filesystem| filesystem.remove_dir(&path))
                .map_err(map_error)
        })
    }

    fn rename<'a>(&'a self, from: &'a str, to: &'a str) -> BackendFuture<'a, Result<(), FsError>> {
        Box::pin(async move {
            let from = little_path(from)?;
            let to = little_path(to)?;
            self.with_fs(|filesystem| filesystem.rename(&from, &to))
                .map_err(map_error)
        })
    }
}

struct LittleFile<S: Storage + Send + 'static> {
    storage: Arc<Mutex<S>>,
    path: String,
    cursor: u64,
    options: OpenOptions,
}

impl<S: Storage + Send + 'static> LittleFile<S> {
    fn with_file<R>(
        &self,
        operation: impl FnOnce(&littlefs2::fs::File<'_, '_, S>) -> littlefs2::io::Result<R>,
    ) -> Result<R, FsError> {
        let path = little_path(&self.path)?;
        let mut storage = self.storage.lock();
        Filesystem::mount_and_then(&mut *storage, |filesystem| {
            filesystem.open_file_with_options_and_then(
                |options| configure(options, &self.options),
                &path,
                operation,
            )
        })
        .map_err(map_error)
    }
}

impl<S: Storage + Send + 'static> BackendFile for LittleFile<S> {
    fn read<'a>(&'a mut self, buffer: &'a mut [u8]) -> BackendFuture<'a, Result<usize, FsError>> {
        Box::pin(async move {
            if !self.options.can_read() {
                return Err(FsError::PermissionDenied);
            }
            let cursor = self.cursor;
            let cursor = u32::try_from(cursor).map_err(|_| FsError::InvalidInput)?;
            let read = self.with_file(|file| {
                file.seek(littlefs2::io::SeekFrom::Start(cursor))?;
                file.read(buffer)
            })?;
            self.cursor = self.cursor.checked_add(read as u64).ok_or(FsError::Io)?;
            Ok(read)
        })
    }

    fn write<'a>(&'a mut self, buffer: &'a [u8]) -> BackendFuture<'a, Result<usize, FsError>> {
        Box::pin(async move {
            if !self.options.can_write() {
                return Err(FsError::PermissionDenied);
            }
            let cursor = self.cursor;
            let cursor = u32::try_from(cursor).map_err(|_| FsError::InvalidInput)?;
            let append = self.options.is_append();
            let written = self.with_file(|file| {
                if append {
                    file.seek(littlefs2::io::SeekFrom::End(0))?;
                } else {
                    file.seek(littlefs2::io::SeekFrom::Start(cursor))?;
                }
                let written = file.write(buffer)?;
                file.sync()?;
                Ok(written)
            })?;
            self.cursor = if append {
                self.with_file(|file| file.len())? as u64
            } else {
                self.cursor.checked_add(written as u64).ok_or(FsError::Io)?
            };
            Ok(written)
        })
    }

    fn flush(&mut self) -> BackendFuture<'_, Result<(), FsError>> {
        Box::pin(async { Ok(()) })
    }

    fn seek(&mut self, position: SeekFrom) -> BackendFuture<'_, Result<u64, FsError>> {
        Box::pin(async move {
            let length = self.with_file(|file| file.len())? as i128;
            let next = match position {
                SeekFrom::Start(offset) => i128::from(offset),
                SeekFrom::End(offset) => length
                    .checked_add(i128::from(offset))
                    .ok_or(FsError::InvalidInput)?,
                SeekFrom::Current(offset) => i128::from(self.cursor)
                    .checked_add(i128::from(offset))
                    .ok_or(FsError::InvalidInput)?,
            };
            if next < 0 || next > i128::from(u64::MAX) {
                return Err(FsError::InvalidInput);
            }
            self.cursor = next as u64;
            Ok(self.cursor)
        })
    }
}

fn little_path(path: &str) -> Result<PathBuf, FsError> {
    PathBuf::try_from(path.as_bytes()).map_err(|_| FsError::InvalidPath)
}

fn configure<'a>(
    options: &'a mut littlefs2::fs::OpenOptions,
    requested: &OpenOptions,
) -> &'a littlefs2::fs::OpenOptions {
    options
        .read(requested.can_read())
        .write(requested.can_write())
        .append(requested.is_append())
        .truncate(requested.should_truncate());
    if requested.is_create_new() {
        options.create_new(true)
    } else {
        options.create(requested.should_create())
    }
}

fn map_error(error: littlefs2::io::Error) -> FsError {
    if error == littlefs2::io::Error::NO_SUCH_ENTRY {
        FsError::NotFound
    } else if error == littlefs2::io::Error::ENTRY_ALREADY_EXISTED {
        FsError::AlreadyExists
    } else if error == littlefs2::io::Error::PATH_NOT_DIR {
        FsError::NotDirectory
    } else if error == littlefs2::io::Error::PATH_IS_DIR {
        FsError::IsDirectory
    } else if error == littlefs2::io::Error::DIR_NOT_EMPTY {
        FsError::DirectoryNotEmpty
    } else if error == littlefs2::io::Error::INVALID {
        FsError::InvalidInput
    } else {
        FsError::Io
    }
}
