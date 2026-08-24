use alloc::vec::Vec;

use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::mutex::Mutex;

use crate::{Backend, File, FsError, Metadata, MountOptions, OpenOptions, ReadDir, Vfs};

static GLOBAL: Mutex<CriticalSectionRawMutex, Vfs> = Mutex::new(Vfs::new());

/// Mounts a backend root into the process-wide VFS namespace.
pub async fn mount(
    mount_point: &str,
    backend: Backend,
    options: MountOptions,
) -> Result<(), FsError> {
    GLOBAL
        .lock()
        .await
        .mount(mount_point, backend, options)
        .await
}

/// Mounts one backend subtree into the process-wide VFS namespace.
pub async fn mount_scoped(
    mount_point: &str,
    backend: Backend,
    source_root: &str,
    options: MountOptions,
) -> Result<(), FsError> {
    GLOBAL
        .lock()
        .await
        .mount_scoped(mount_point, backend, source_root, options)
        .await
}

/// Removes an exact process-wide mount point.
pub async fn unmount(mount_point: &str) -> Result<(), FsError> {
    GLOBAL.lock().await.unmount(mount_point).await
}

/// Opens an existing global VFS file for reading.
pub async fn open(path: &str) -> Result<File, FsError> {
    GLOBAL.lock().await.open(path).await
}

/// Creates or truncates a global VFS file.
pub async fn create(path: &str) -> Result<File, FsError> {
    GLOBAL.lock().await.create(path).await
}

/// Opens a global VFS file with explicit options.
pub async fn open_with(path: &str, options: &OpenOptions) -> Result<File, FsError> {
    GLOBAL.lock().await.open_with(path, options).await
}

/// Reads an entire global VFS file.
pub async fn read(path: &str) -> Result<Vec<u8>, FsError> {
    GLOBAL.lock().await.read(path).await
}

/// Replaces a global VFS file with `bytes`.
pub async fn write(path: &str, bytes: &[u8]) -> Result<(), FsError> {
    GLOBAL.lock().await.write(path, bytes).await
}

/// Returns metadata from the global VFS.
pub async fn metadata(path: &str) -> Result<Metadata, FsError> {
    GLOBAL.lock().await.metadata(path).await
}

/// Lists a directory from the global VFS.
pub async fn read_dir(path: &str) -> Result<ReadDir, FsError> {
    GLOBAL.lock().await.read_dir(path).await
}

/// Recursively creates a directory in the global VFS.
pub async fn create_dir_all(path: &str) -> Result<(), FsError> {
    GLOBAL.lock().await.create_dir_all(path).await
}

/// Removes one global VFS file.
pub async fn remove_file(path: &str) -> Result<(), FsError> {
    GLOBAL.lock().await.remove_file(path).await
}

/// Removes one empty global VFS directory.
pub async fn remove_dir(path: &str) -> Result<(), FsError> {
    GLOBAL.lock().await.remove_dir(path).await
}

/// Renames a path in the global VFS.
pub async fn rename(from: &str, to: &str) -> Result<(), FsError> {
    GLOBAL.lock().await.rename(from, to).await
}
