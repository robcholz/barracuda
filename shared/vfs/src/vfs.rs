use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicUsize, Ordering};

use embedded_io_async::{Read, Write};

use crate::path::{backend_path, matches_mount, normalize};
use crate::{Backend, DirEntry, File, FsError, Metadata, MountOptions, OpenOptions, ReadDir};

#[derive(Clone)]
struct Mount {
    point: String,
    source_root: String,
    backend: Backend,
    options: MountOptions,
    open_files: Arc<AtomicUsize>,
}

struct Resolved {
    mount: Mount,
    path: String,
}

/// One independent virtual-filesystem mount namespace.
#[derive(Clone)]
pub struct Vfs {
    mounts: Vec<Mount>,
}

impl Vfs {
    /// Creates an empty namespace with no implicit fallback filesystem.
    pub const fn new() -> Self {
        Self { mounts: Vec::new() }
    }

    /// Creates a mount-management-free view rooted beneath `root`.
    pub fn scoped(&self, root: &str) -> Result<crate::ScopedVfs, FsError> {
        let root = normalize(root)?;
        Ok(crate::ScopedVfs::new(self.clone(), root))
    }

    /// Mounts a backend root at `mount_point`.
    pub async fn mount(
        &mut self,
        mount_point: &str,
        backend: Backend,
        options: MountOptions,
    ) -> Result<(), FsError> {
        self.mount_scoped(mount_point, backend, "/", options).await
    }

    /// Mounts `source_root` from a backend at `mount_point`.
    pub async fn mount_scoped(
        &mut self,
        mount_point: &str,
        backend: Backend,
        source_root: &str,
        options: MountOptions,
    ) -> Result<(), FsError> {
        let point = normalize(mount_point)?;
        let source_root = normalize(source_root)?;
        if self.mounts.iter().any(|mount| mount.point == point) {
            return Err(FsError::MountConflict);
        }
        backend
            .inner
            .metadata(&source_root)
            .await
            .and_then(|metadata| {
                if metadata.is_dir() {
                    Ok(())
                } else {
                    Err(FsError::NotDirectory)
                }
            })?;
        self.mounts.push(Mount {
            point,
            source_root,
            backend,
            options,
            open_files: Arc::new(AtomicUsize::new(0)),
        });
        Ok(())
    }

    /// Removes an exact mount point when it owns no open files.
    pub async fn unmount(&mut self, mount_point: &str) -> Result<(), FsError> {
        let point = normalize(mount_point)?;
        let index = self
            .mounts
            .iter()
            .position(|mount| mount.point == point)
            .ok_or(FsError::NotMounted)?;
        if self.mounts[index].open_files.load(Ordering::Acquire) != 0 {
            return Err(FsError::Busy);
        }
        self.mounts.remove(index);
        Ok(())
    }

    /// Opens an existing file for reading.
    pub async fn open(&self, path: &str) -> Result<File, FsError> {
        self.open_with(path, &OpenOptions::read_only()).await
    }

    /// Creates or truncates a file for writing.
    pub async fn create(&self, path: &str) -> Result<File, FsError> {
        self.open_with(path, &OpenOptions::create_truncate()).await
    }

    /// Opens a file with explicit access and creation options.
    pub async fn open_with(&self, path: &str, options: &OpenOptions) -> Result<File, FsError> {
        if !options.validates() {
            return Err(FsError::InvalidInput);
        }
        let resolved = self.resolve(path)?;
        resolved.ensure_writable(options.mutates())?;
        let file = resolved
            .mount
            .backend
            .inner
            .open(&resolved.path, options)
            .await?;
        Ok(File::new(file, resolved.mount.open_files))
    }

    /// Reads an entire file into a new byte vector.
    pub async fn read(&self, path: &str) -> Result<Vec<u8>, FsError> {
        let mut file = self.open(path).await?;
        let capacity =
            usize::try_from(self.metadata(path).await?.len()).map_err(|_| FsError::Io)?;
        let mut bytes = alloc::vec![0; capacity];
        let mut filled = 0;
        while filled < bytes.len() {
            let read = file.read(&mut bytes[filled..]).await?;
            if read == 0 {
                bytes.truncate(filled);
                return Ok(bytes);
            }
            filled += read;
        }

        // Preserve normal file semantics if a file grows after metadata was
        // sampled. Stable static assets complete in the single read above.
        let mut chunk = [0_u8; 1024];
        loop {
            let read = file.read(&mut chunk).await?;
            if read == 0 {
                break;
            }
            bytes.extend_from_slice(&chunk[..read]);
        }
        Ok(bytes)
    }

    /// Replaces a file with `bytes`, creating it when absent.
    pub async fn write(&self, path: &str, bytes: &[u8]) -> Result<(), FsError> {
        let mut file = self.create(path).await?;
        file.write_all(bytes).await?;
        file.flush().await
    }

    /// Returns metadata for a mounted path.
    pub async fn metadata(&self, path: &str) -> Result<Metadata, FsError> {
        let resolved = self.resolve(path)?;
        resolved.mount.backend.inner.metadata(&resolved.path).await
    }

    /// Lists immediate children of a mounted directory.
    pub async fn read_dir(&self, path: &str) -> Result<ReadDir, FsError> {
        let resolved = self.resolve(path)?;
        resolved
            .mount
            .backend
            .inner
            .read_dir(&resolved.path)
            .await
            .map(ReadDir::new)
    }

    /// Recursively creates a directory and missing ancestors.
    pub async fn create_dir_all(&self, path: &str) -> Result<(), FsError> {
        let resolved = self.resolve(path)?;
        resolved.ensure_writable(true)?;
        resolved
            .mount
            .backend
            .inner
            .create_dir_all(&resolved.path)
            .await
    }

    /// Removes one regular file.
    pub async fn remove_file(&self, path: &str) -> Result<(), FsError> {
        let resolved = self.resolve(path)?;
        resolved.ensure_writable(true)?;
        resolved
            .mount
            .backend
            .inner
            .remove_file(&resolved.path)
            .await
    }

    /// Removes one empty directory.
    pub async fn remove_dir(&self, path: &str) -> Result<(), FsError> {
        let resolved = self.resolve(path)?;
        resolved.ensure_writable(true)?;
        resolved
            .mount
            .backend
            .inner
            .remove_dir(&resolved.path)
            .await
    }

    /// Renames a path without crossing a mount boundary.
    pub async fn rename(&self, from: &str, to: &str) -> Result<(), FsError> {
        let from = self.resolve(from)?;
        let to = self.resolve(to)?;
        if !Arc::ptr_eq(&from.mount.open_files, &to.mount.open_files) {
            return Err(FsError::CrossMount);
        }
        from.ensure_writable(true)?;
        to.ensure_writable(true)?;
        from.mount.backend.inner.rename(&from.path, &to.path).await
    }

    fn resolve(&self, path: &str) -> Result<Resolved, FsError> {
        let normalized = normalize(path)?;
        let mount = self
            .mounts
            .iter()
            .filter(|mount| matches_mount(&normalized, &mount.point))
            .max_by_key(|mount| mount.point.len())
            .cloned()
            .ok_or(FsError::NotMounted)?;
        let path = backend_path(&normalized, &mount.point, &mount.source_root);
        Ok(Resolved { mount, path })
    }
}

impl Default for Vfs {
    fn default() -> Self {
        Self::new()
    }
}

impl Resolved {
    fn ensure_writable(&self, mutation: bool) -> Result<(), FsError> {
        if mutation && self.mount.options.is_read_only() {
            Err(FsError::ReadOnly)
        } else {
            Ok(())
        }
    }
}

#[allow(dead_code)]
fn _keep_dir_entry_documented(_: DirEntry) {}
