use alloc::borrow::Cow;
use alloc::string::String;
use alloc::vec::Vec;
use core::cell::RefCell;

use embassy_sync::blocking_mutex::{raw::CriticalSectionRawMutex, Mutex};
use embedded_io_async::{Read, Write};
use portable_atomic::{AtomicUsize, Ordering};
use portable_atomic_util::Arc;

use crate::path::{backend_path, matches_mount, normalize, normalize_retained};
use crate::{
    Backend, DirEntry, File, FileType, FsError, Metadata, MountOptions, OpenOptions, ReadDir,
};

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
///
/// Clones share one live mount registry. Mounts and unmounts performed through
/// any clone are therefore visible to every scoped view derived from this
/// namespace.
#[derive(Clone)]
pub struct Vfs {
    mounts: Arc<Mutex<CriticalSectionRawMutex, RefCell<Vec<Mount>>>>,
}

impl Vfs {
    /// Creates an empty namespace with no implicit fallback filesystem.
    pub fn new() -> Self {
        Self {
            mounts: Arc::new(Mutex::new(RefCell::new(Vec::new()))),
        }
    }

    /// Creates a mount-management-free view rooted beneath `root`.
    pub fn scoped(&self, root: &str) -> Result<crate::ScopedVfs, FsError> {
        let root = normalize(root)?;
        Ok(crate::ScopedVfs::with_mounts(
            self.clone(),
            alloc::vec![crate::scoped::ScopedMount::new(
                Cow::Borrowed("/"),
                Cow::Owned(root),
                MountOptions::read_write(),
            )],
        ))
    }

    /// Creates a mount-management-free view with independent logical roots.
    ///
    /// # Errors
    ///
    /// Returns [`FsError::InvalidPath`] for an invalid logical mount point or
    /// source root, and [`FsError::MountConflict`] when normalized logical
    /// mount points collide.
    ///
    /// The view keeps its paths for its whole lifetime: static paths that are
    /// already normalized are kept borrowed instead of copied.
    pub fn scoped_mounts<I, Point, Source>(&self, mounts: I) -> Result<crate::ScopedVfs, FsError>
    where
        I: IntoIterator<Item = (Point, Source)>,
        Point: Into<Cow<'static, str>>,
        Source: Into<Cow<'static, str>>,
    {
        self.scoped_mounts_with(
            mounts
                .into_iter()
                .map(|(point, source)| (point, source, MountOptions::read_write())),
        )
    }

    /// Creates a mount-management-free view whose logical roots each carry
    /// their own access policy.
    ///
    /// A read-only mount refuses every change through the view with
    /// [`FsError::ReadOnly`], even when its source is writable.
    ///
    /// # Errors
    ///
    /// The same as [`Self::scoped_mounts`].
    pub fn scoped_mounts_with<I, Point, Source>(
        &self,
        mounts: I,
    ) -> Result<crate::ScopedVfs, FsError>
    where
        I: IntoIterator<Item = (Point, Source, MountOptions)>,
        Point: Into<Cow<'static, str>>,
        Source: Into<Cow<'static, str>>,
    {
        let mounts = mounts.into_iter();
        let mut scoped_mounts = Vec::with_capacity(mounts.size_hint().0);
        for (point, source_root, options) in mounts {
            let point = normalize_retained(point.into())?;
            if scoped_mounts
                .iter()
                .any(|mount: &crate::scoped::ScopedMount| mount.point == point)
            {
                return Err(FsError::MountConflict);
            }
            let source_root = normalize_retained(source_root.into())?;
            scoped_mounts.push(crate::scoped::ScopedMount::new(point, source_root, options));
        }
        Ok(crate::ScopedVfs::with_mounts(self.clone(), scoped_mounts))
    }

    /// Mounts a backend root at `mount_point`.
    pub async fn mount(
        &self,
        mount_point: &str,
        backend: Backend,
        options: MountOptions,
    ) -> Result<(), FsError> {
        self.mount_scoped(mount_point, backend, "/", options).await
    }

    /// Mounts `source_root` from a backend at `mount_point`.
    pub async fn mount_scoped(
        &self,
        mount_point: &str,
        backend: Backend,
        source_root: &str,
        options: MountOptions,
    ) -> Result<(), FsError> {
        let point = normalize(mount_point)?;
        let source_root = normalize(source_root)?;
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
        self.mounts.lock(|mounts| {
            let mut mounts = mounts.borrow_mut();
            if mounts.iter().any(|mount| mount.point == point) {
                return Err(FsError::MountConflict);
            }
            mounts.push(Mount {
                point,
                source_root,
                backend,
                options,
                open_files: Arc::new(AtomicUsize::new(0)),
            });
            Ok(())
        })
    }

    /// Removes an exact mount point when it owns no open files.
    pub async fn unmount(&self, mount_point: &str) -> Result<(), FsError> {
        let point = normalize(mount_point)?;
        self.mounts.lock(|mounts| {
            let mut mounts = mounts.borrow_mut();
            let index = mounts
                .iter()
                .position(|mount| mount.point == point)
                .ok_or(FsError::NotMounted)?;
            let mount = mounts.get(index).ok_or(FsError::NotMounted)?;
            if mount.open_files.load(Ordering::Acquire) != 0 {
                return Err(FsError::Busy);
            }
            mounts.remove(index);
            Ok(())
        })
    }

    /// Detaches an exact mount point while existing file handles retain their backend.
    ///
    /// New path resolution stops reaching the backend immediately. Existing
    /// handles remain owned by their callers and observe any backend-level
    /// failure independently.
    pub async fn detach(&self, mount_point: &str) -> Result<(), FsError> {
        let point = normalize(mount_point)?;
        self.mounts.lock(|mounts| {
            let mut mounts = mounts.borrow_mut();
            let index = mounts
                .iter()
                .position(|mount| mount.point == point)
                .ok_or(FsError::NotMounted)?;
            mounts.remove(index);
            Ok(())
        })
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
        let mut entries = resolved
            .mount
            .backend
            .inner
            .read_dir(&resolved.path)
            .await?;
        let normalized = normalize(path)?;
        for name in self.immediate_mount_children(&normalized) {
            let mounted = DirEntry::new(name.as_str(), Metadata::new(FileType::Directory, 0));
            if let Some(entry) = entries.iter_mut().find(|entry| entry.file_name() == name) {
                *entry = mounted;
            } else {
                entries.push(mounted);
            }
        }
        Ok(ReadDir::new(entries))
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
        // A directory cannot become its own descendant: backends detach the
        // source before they look up the destination's parent inside it.
        if to
            .path
            .strip_prefix(from.path.as_str())
            .is_some_and(|rest| rest.starts_with('/'))
        {
            return Err(FsError::InvalidInput);
        }
        from.mount.backend.inner.rename(&from.path, &to.path).await
    }

    fn resolve(&self, path: &str) -> Result<Resolved, FsError> {
        let normalized = normalize(path)?;
        let mount = self.mounts.lock(|mounts| {
            mounts
                .borrow()
                .iter()
                .filter(|mount| matches_mount(&normalized, &mount.point))
                .max_by_key(|mount| mount.point.len())
                .cloned()
        });
        let mount = mount.ok_or(FsError::NotMounted)?;
        let path = backend_path(&normalized, &mount.point, &mount.source_root);
        Ok(Resolved { mount, path })
    }

    fn immediate_mount_children(&self, parent: &str) -> Vec<String> {
        self.mounts.lock(|mounts| {
            let mut children = Vec::new();
            for mount in mounts.borrow().iter() {
                let relative = if parent == "/" {
                    mount.point.strip_prefix('/')
                } else {
                    mount
                        .point
                        .strip_prefix(parent)
                        .and_then(|path| path.strip_prefix('/'))
                };
                let Some(relative) = relative.filter(|relative| !relative.is_empty()) else {
                    continue;
                };
                let name = relative.split('/').next().unwrap_or(relative);
                if children.iter().all(|child| child != name) {
                    children.push(String::from(name));
                }
            }
            children
        })
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
