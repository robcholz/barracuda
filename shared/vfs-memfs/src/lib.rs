#![no_std]

//! In-memory backend for `barracuda-vfs`.

extern crate alloc;

use alloc::boxed::Box;
use alloc::string::{String, ToString};
use alloc::sync::Arc;
use alloc::vec::Vec;

use bare_vfs::{MemFs as BareMemFs, VfsError as BareError, VfsErrorKind};
use barracuda_vfs::{
    Backend, BackendFile, BackendFuture, DirEntry, FileType, FsError, Metadata, OpenOptions,
    SeekFrom, VfsBackend,
};
use spin::Mutex;

/// A cloneable in-memory filesystem backed by `bare-vfs`.
#[derive(Clone)]
pub struct MemFs {
    inner: Arc<Mutex<BareMemFs>>,
}

impl MemFs {
    /// Creates an empty filesystem containing only its root directory.
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(BareMemFs::new())),
        }
    }

    /// Erases this filesystem for mounting into a VFS namespace.
    pub fn into_backend(self) -> Backend {
        Backend::new(self)
    }

    /// Recursively creates a directory directly in this backend.
    pub fn create_dir_all(&self, path: &str) -> Result<(), FsError> {
        self.inner.lock().create_dir_all(path).map_err(map_error)
    }

    /// Replaces one file directly in this backend.
    pub fn write_file(&self, path: &str, bytes: &[u8]) -> Result<(), FsError> {
        self.inner
            .lock()
            .write(path, bytes.to_vec())
            .map_err(map_error)
    }
}

impl Default for MemFs {
    fn default() -> Self {
        Self::new()
    }
}

impl VfsBackend for MemFs {
    fn open<'a>(
        &'a self,
        path: &'a str,
        options: &'a OpenOptions,
    ) -> BackendFuture<'a, Result<Box<dyn BackendFile>, FsError>> {
        Box::pin(async move {
            let mut filesystem = self.inner.lock();
            let exists = filesystem.exists(path);
            if options.is_create_new() && exists {
                return Err(FsError::AlreadyExists);
            }
            if !exists && !options.should_create() {
                return Err(FsError::NotFound);
            }
            if exists && filesystem.is_dir(path) {
                return Err(FsError::IsDirectory);
            }
            if !exists || options.should_truncate() {
                filesystem.write(path, Vec::new()).map_err(map_error)?;
            }
            let cursor = if options.is_append() {
                u64::try_from(filesystem.read(path).map_err(map_error)?.len())
                    .map_err(|_| FsError::Io)?
            } else {
                0
            };
            drop(filesystem);

            Ok(Box::new(MemFile {
                filesystem: self.inner.clone(),
                path: path.to_string(),
                cursor,
                readable: options.can_read(),
                writable: options.can_write(),
                append: options.is_append(),
            }) as Box<dyn BackendFile>)
        })
    }

    fn metadata<'a>(&'a self, path: &'a str) -> BackendFuture<'a, Result<Metadata, FsError>> {
        Box::pin(async move {
            let metadata = self.inner.lock().metadata(path).map_err(map_error)?;
            let file_type = if metadata.is_file() {
                FileType::File
            } else if metadata.is_dir() {
                FileType::Directory
            } else if metadata.is_symlink() {
                FileType::Symlink
            } else {
                FileType::Other
            };
            let len = u64::try_from(metadata.len()).map_err(|_| FsError::Io)?;
            Ok(Metadata::new(file_type, len))
        })
    }

    fn read_dir<'a>(&'a self, path: &'a str) -> BackendFuture<'a, Result<Vec<DirEntry>, FsError>> {
        Box::pin(async move {
            self.inner
                .lock()
                .read_dir(path)
                .map_err(map_error)?
                .into_iter()
                .map(|entry| {
                    let file_type = if entry.is_dir {
                        FileType::Directory
                    } else if entry.is_symlink {
                        FileType::Symlink
                    } else {
                        FileType::File
                    };
                    let len = u64::try_from(entry.size).map_err(|_| FsError::Io)?;
                    Ok(DirEntry::new(entry.name, Metadata::new(file_type, len)))
                })
                .collect()
        })
    }

    fn create_dir_all<'a>(&'a self, path: &'a str) -> BackendFuture<'a, Result<(), FsError>> {
        Box::pin(async move { MemFs::create_dir_all(self, path) })
    }

    fn remove_file<'a>(&'a self, path: &'a str) -> BackendFuture<'a, Result<(), FsError>> {
        Box::pin(async move { self.inner.lock().remove_file(path).map_err(map_error) })
    }

    fn remove_dir<'a>(&'a self, path: &'a str) -> BackendFuture<'a, Result<(), FsError>> {
        Box::pin(async move { self.inner.lock().remove_dir(path).map_err(map_error) })
    }

    fn rename<'a>(&'a self, from: &'a str, to: &'a str) -> BackendFuture<'a, Result<(), FsError>> {
        Box::pin(async move { self.inner.lock().rename(from, to).map_err(map_error) })
    }
}

struct MemFile {
    filesystem: Arc<Mutex<BareMemFs>>,
    path: String,
    cursor: u64,
    readable: bool,
    writable: bool,
    append: bool,
}

impl BackendFile for MemFile {
    fn read<'a>(&'a mut self, buffer: &'a mut [u8]) -> BackendFuture<'a, Result<usize, FsError>> {
        Box::pin(async move {
            if !self.readable {
                return Err(FsError::PermissionDenied);
            }
            let filesystem = self.filesystem.lock();
            let bytes = filesystem.read(&self.path).map_err(map_error)?;
            let start = usize::try_from(self.cursor).map_err(|_| FsError::InvalidInput)?;
            if start >= bytes.len() {
                return Ok(0);
            }
            let amount = buffer.len().min(bytes.len() - start);
            buffer[..amount].copy_from_slice(&bytes[start..start + amount]);
            self.cursor = self
                .cursor
                .checked_add(u64::try_from(amount).map_err(|_| FsError::Io)?)
                .ok_or(FsError::Io)?;
            Ok(amount)
        })
    }

    fn write<'a>(&'a mut self, buffer: &'a [u8]) -> BackendFuture<'a, Result<usize, FsError>> {
        Box::pin(async move {
            if !self.writable {
                return Err(FsError::PermissionDenied);
            }
            let mut filesystem = self.filesystem.lock();
            let mut bytes = filesystem.read(&self.path).map_err(map_error)?.to_vec();
            let start = if self.append {
                bytes.len()
            } else {
                usize::try_from(self.cursor).map_err(|_| FsError::InvalidInput)?
            };
            let end = start.checked_add(buffer.len()).ok_or(FsError::Io)?;
            if end > bytes.len() {
                bytes.resize(end, 0);
            }
            bytes[start..end].copy_from_slice(buffer);
            filesystem.write(&self.path, bytes).map_err(map_error)?;
            self.cursor = u64::try_from(end).map_err(|_| FsError::Io)?;
            Ok(buffer.len())
        })
    }

    fn flush(&mut self) -> BackendFuture<'_, Result<(), FsError>> {
        Box::pin(async { Ok(()) })
    }

    fn seek(&mut self, position: SeekFrom) -> BackendFuture<'_, Result<u64, FsError>> {
        Box::pin(async move {
            let length = i128::try_from(
                self.filesystem
                    .lock()
                    .read(&self.path)
                    .map_err(map_error)?
                    .len(),
            )
            .map_err(|_| FsError::Io)?;
            let current = i128::from(self.cursor);
            let next = match position {
                SeekFrom::Start(offset) => i128::from(offset),
                SeekFrom::End(offset) => length
                    .checked_add(i128::from(offset))
                    .ok_or(FsError::InvalidInput)?,
                SeekFrom::Current(offset) => current
                    .checked_add(i128::from(offset))
                    .ok_or(FsError::InvalidInput)?,
            };
            if next < 0 || next > i128::from(u64::MAX) {
                return Err(FsError::InvalidInput);
            }
            self.cursor = u64::try_from(next).map_err(|_| FsError::InvalidInput)?;
            Ok(self.cursor)
        })
    }
}

fn map_error(error: BareError) -> FsError {
    match error.kind() {
        VfsErrorKind::NotFound => FsError::NotFound,
        VfsErrorKind::IsADirectory => FsError::IsDirectory,
        VfsErrorKind::NotADirectory => FsError::NotDirectory,
        VfsErrorKind::PermissionDenied => FsError::PermissionDenied,
        VfsErrorKind::AlreadyExists => FsError::AlreadyExists,
        VfsErrorKind::DirectoryNotEmpty => FsError::DirectoryNotEmpty,
        _ => FsError::Io,
    }
}
