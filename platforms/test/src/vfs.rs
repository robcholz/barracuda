//! Deterministic in-memory VFS namespaces for tests.

use barracuda_vfs::{mount, FsError, MountOptions, ScopedVfs, Vfs};

/// Creates one empty writable root VFS, including mount-table ownership.
///
/// Tests that exercise System or Plugin Manager composition use this form;
/// ordinary consumers should prefer [`memory_vfs`].
pub async fn memory_vfs_root() -> Result<Vfs, FsError> {
    let vfs = Vfs::new();
    vfs.mount(
        "/",
        barracuda_vfs_memfs::MemFs::new().into_backend(),
        MountOptions::read_write(),
    )
    .await?;
    Ok(vfs)
}

/// Creates one empty writable VFS namespace.
///
/// # Errors
///
/// Returns an error if the in-memory backend root cannot be mounted.
pub async fn memory_vfs() -> Result<ScopedVfs, FsError> {
    let vfs = memory_vfs_root().await?;
    vfs.scoped("/")
}

/// Installs an in-memory backend at the process-wide VFS root.
///
/// A mount conflict is accepted so independently constructed test services can
/// share the same process-wide namespace, matching production System behavior.
///
/// # Errors
///
/// Returns an error when the global root cannot be mounted for a reason other
/// than an existing root mount.
pub async fn install_global_memory_vfs() -> Result<(), FsError> {
    match mount(
        "/",
        barracuda_vfs_memfs::MemFs::new().into_backend(),
        MountOptions::read_write(),
    )
    .await
    {
        Ok(()) | Err(FsError::MountConflict) => Ok(()),
        Err(error) => Err(error),
    }
}
