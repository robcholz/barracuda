use alloc::boxed::Box;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::fmt;
use core::future::Future;
use core::pin::Pin;
use core::sync::atomic::{AtomicUsize, Ordering};

use embedded_io::{ErrorType, SeekFrom};
use embedded_io_async::{Read, Seek, Write};

use crate::{DirEntry, FsError, Metadata, OpenOptions};

/// A boxed backend operation that may yield while hardware I/O is pending.
pub type BackendFuture<'a, T> = Pin<Box<dyn Future<Output = T> + 'a>>;

/// Backend-owned asynchronous open-file operations erased behind [`File`].
pub trait BackendFile {
    /// Reads bytes from the current cursor.
    fn read<'a>(&'a mut self, buffer: &'a mut [u8]) -> BackendFuture<'a, Result<usize, FsError>>;
    /// Writes bytes at the current cursor.
    fn write<'a>(&'a mut self, buffer: &'a [u8]) -> BackendFuture<'a, Result<usize, FsError>>;
    /// Flushes buffered file and metadata changes.
    fn flush(&mut self) -> BackendFuture<'_, Result<(), FsError>>;
    /// Moves the file cursor.
    fn seek(&mut self, position: SeekFrom) -> BackendFuture<'_, Result<u64, FsError>>;
}

/// An asynchronous filesystem implementation mountable into a [`crate::Vfs`].
pub trait VfsBackend: Send + Sync + 'static {
    /// Opens a regular file relative to the backend root.
    fn open<'a>(
        &'a self,
        path: &'a str,
        options: &'a OpenOptions,
    ) -> BackendFuture<'a, Result<Box<dyn BackendFile>, FsError>>;
    /// Returns metadata for a path.
    fn metadata<'a>(&'a self, path: &'a str) -> BackendFuture<'a, Result<Metadata, FsError>>;
    /// Lists immediate directory children.
    fn read_dir<'a>(&'a self, path: &'a str) -> BackendFuture<'a, Result<Vec<DirEntry>, FsError>>;
    /// Recursively creates a directory and missing ancestors.
    fn create_dir_all<'a>(&'a self, path: &'a str) -> BackendFuture<'a, Result<(), FsError>>;
    /// Removes one regular file.
    fn remove_file<'a>(&'a self, path: &'a str) -> BackendFuture<'a, Result<(), FsError>>;
    /// Removes one empty directory.
    fn remove_dir<'a>(&'a self, path: &'a str) -> BackendFuture<'a, Result<(), FsError>>;
    /// Renames a path within this backend.
    fn rename<'a>(&'a self, from: &'a str, to: &'a str) -> BackendFuture<'a, Result<(), FsError>>;
}

/// Cloneable type-erased ownership of a mounted filesystem backend.
#[derive(Clone)]
pub struct Backend {
    pub(crate) inner: Arc<dyn VfsBackend>,
}

impl Backend {
    /// Erases a concrete backend for mounting and sharing between namespaces.
    pub fn new(backend: impl VfsBackend) -> Self {
        Self {
            inner: Arc::new(backend),
        }
    }
}

/// A normal open file returned by the VFS.
pub struct File {
    inner: Box<dyn BackendFile>,
    open_files: Arc<AtomicUsize>,
}

impl fmt::Debug for File {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_struct("File").finish_non_exhaustive()
    }
}

impl File {
    pub(crate) fn new(inner: Box<dyn BackendFile>, open_files: Arc<AtomicUsize>) -> Self {
        open_files.fetch_add(1, Ordering::Relaxed);
        Self { inner, open_files }
    }
}

impl Drop for File {
    fn drop(&mut self) {
        self.open_files.fetch_sub(1, Ordering::Release);
    }
}

impl ErrorType for File {
    type Error = FsError;
}

impl Read for File {
    async fn read(&mut self, buffer: &mut [u8]) -> Result<usize, Self::Error> {
        self.inner.read(buffer).await
    }
}

impl Write for File {
    async fn write(&mut self, buffer: &[u8]) -> Result<usize, Self::Error> {
        self.inner.write(buffer).await
    }

    async fn flush(&mut self) -> Result<(), Self::Error> {
        self.inner.flush().await
    }
}

impl Seek for File {
    async fn seek(&mut self, position: SeekFrom) -> Result<u64, Self::Error> {
        self.inner.seek(position).await
    }
}
