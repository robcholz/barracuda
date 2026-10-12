use alloc::borrow::Cow;
use alloc::string::String;
use alloc::vec::Vec;

use embedded_io_async::{Read, Seek, Write};
use portable_atomic::{AtomicUsize, Ordering};
use portable_atomic_util::Arc;

use crate::path::{backend_path, matches_mount, normalize};
use crate::{
    DirEntry, File, FileType, FsError, Metadata, MountOptions, OpenOptions, ReadDir, SeekFrom, Vfs,
};

static TEMP_FILE_SEQUENCE: AtomicUsize = AtomicUsize::new(0);

/// A cloneable filesystem view exposing selected paths from an existing VFS.
///
/// It exposes file operations only; mount-table ownership remains with the
/// [`Vfs`] that created the view. A view's mounts never change after it is
/// created, so clones share them instead of copying every path.
///
/// Every mount point is a directory of the view: it lists as empty until its
/// source exists and cannot be written, removed, or renamed. The parents of
/// mount points, such as `/` or `/workspace`, list the mount points below them.
///
/// A mount may be read-only in the view even when its source is writable:
/// every operation that would change it fails with [`FsError::ReadOnly`].
#[derive(Clone)]
pub struct ScopedVfs {
    vfs: Vfs,
    mounts: Arc<[ScopedMount]>,
}

impl ScopedVfs {
    pub(crate) fn with_mounts(vfs: Vfs, mounts: Vec<ScopedMount>) -> Self {
        Self {
            vfs,
            mounts: Arc::from(mounts),
        }
    }

    fn path(&self, path: &str) -> Result<String, FsError> {
        match self.locate(path)? {
            Location::Mounted { path, .. } => Ok(path),
            Location::Above(_) => Err(FsError::NotMounted),
        }
    }

    /// The source path of a view path whose mount may be changed.
    fn writable_path(&self, path: &str) -> Result<String, FsError> {
        match self.locate(path)? {
            Location::Mounted {
                read_only: true, ..
            } => Err(FsError::ReadOnly),
            Location::Mounted { path, .. } => Ok(path),
            Location::Above(_) => Err(FsError::NotMounted),
        }
    }

    /// The source path of a view path that is not itself a mount point and
    /// whose mount may be changed.
    fn entry_path(&self, path: &str, at_mount_point: FsError) -> Result<String, FsError> {
        match self.locate(path)? {
            Location::Mounted {
                read_only: true, ..
            } => Err(FsError::ReadOnly),
            Location::Mounted { root: true, .. } => Err(at_mount_point),
            Location::Mounted { path, .. } => Ok(path),
            Location::Above(_) => Err(at_mount_point),
        }
    }

    fn locate(&self, path: &str) -> Result<Location, FsError> {
        let path = if path.starts_with('/') {
            normalize(path)?
        } else {
            let mut relative = String::with_capacity(path.len().saturating_add(1));
            relative.push('/');
            relative.push_str(path);
            normalize(&relative)?
        };
        if let Some(mount) = self
            .mounts
            .iter()
            .filter(|mount| matches_mount(&path, &mount.point))
            .max_by_key(|mount| mount.point.len())
        {
            return Ok(Location::Mounted {
                root: path == mount.point,
                read_only: mount.read_only,
                path: backend_path(&path, &mount.point, &mount.source_root),
            });
        }
        if self
            .mounts
            .iter()
            .any(|mount| matches_mount(&mount.point, &path))
        {
            return Ok(Location::Above(path));
        }
        Err(FsError::NotMounted)
    }

    /// The mount-point directories directly below `parent`, which is above
    /// every mount point it lists.
    fn mount_point_entries(&self, parent: &str) -> Vec<DirEntry> {
        let mut names: Vec<&str> = self
            .mounts
            .iter()
            .filter_map(|mount| {
                let below = if parent == "/" {
                    mount.point.strip_prefix('/')
                } else {
                    mount.point.strip_prefix(parent)?.strip_prefix('/')
                }?;
                below.split('/').next().filter(|name| !name.is_empty())
            })
            .collect();
        names.sort_unstable();
        names.dedup();
        names
            .into_iter()
            .map(|name| DirEntry::new(name, DIRECTORY))
            .collect()
    }

    async fn prepare_file_path(&self, path: &str) -> Result<String, FsError> {
        let path = self.entry_path(path, FsError::IsDirectory)?;
        if let Some((parent, _name)) = path.rsplit_once('/') {
            let parent = if parent.is_empty() { "/" } else { parent };
            self.vfs.create_dir_all(parent).await?;
        }
        Ok(path)
    }

    /// Opens an existing file for reading.
    pub async fn open(&self, path: &str) -> Result<File, FsError> {
        self.vfs.open(&self.path(path)?).await
    }

    /// Creates or truncates a file for writing.
    pub async fn create(&self, path: &str) -> Result<File, FsError> {
        let path = self.prepare_file_path(path).await?;
        self.vfs.create(&path).await
    }

    /// Opens a file with explicit options.
    pub async fn open_with(&self, path: &str, options: &OpenOptions) -> Result<File, FsError> {
        let path = if options.should_create() {
            self.prepare_file_path(path).await?
        } else if options.mutates() {
            self.entry_path(path, FsError::IsDirectory)?
        } else {
            self.path(path)?
        };
        self.vfs.open_with(&path, options).await
    }

    /// Reads an entire file.
    pub async fn read(&self, path: &str) -> Result<Vec<u8>, FsError> {
        self.vfs.read(&self.path(path)?).await
    }

    /// Replaces a file, creating it when absent.
    pub async fn write(&self, path: &str, bytes: &[u8]) -> Result<(), FsError> {
        let path = self.prepare_file_path(path).await?;
        self.vfs.write(&path, bytes).await
    }

    /// Durably replaces a file through a same-filesystem temporary rename.
    pub async fn write_atomic(&self, path: &str, bytes: &[u8]) -> Result<(), FsError> {
        let target = self.prepare_file_path(path).await?;
        let sequence = TEMP_FILE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        // A sibling with its own short name: appending to the target's name
        // would push a name near the backend's length limit past it.
        let parent = target.rsplit_once('/').map_or("", |(parent, _name)| parent);
        let temporary = alloc::format!("{parent}/.barracuda-tmp-{sequence}");
        if let Err(error) = self.vfs.write(&temporary, bytes).await {
            let _ignored = self.vfs.remove_file(&temporary).await;
            return Err(error);
        }
        if let Err(error) = self.vfs.rename(&temporary, &target).await {
            let _ignored = self.vfs.remove_file(&temporary).await;
            return Err(error);
        }
        Ok(())
    }

    /// Appends bytes, creating the file when absent.
    pub async fn append(&self, path: &str, bytes: &[u8]) -> Result<(), FsError> {
        let path = self.prepare_file_path(path).await?;
        let mut options = OpenOptions::new();
        options.write(true).append(true).create(true);
        let mut file = self.vfs.open_with(&path, &options).await?;
        file.write_all(bytes).await?;
        file.flush().await
    }

    /// Reads exactly `len` bytes starting at `offset`.
    pub async fn read_at(&self, path: &str, offset: u64, len: usize) -> Result<Vec<u8>, FsError> {
        let mut file = self.open(path).await?;
        file.seek(SeekFrom::Start(offset)).await?;
        let mut bytes = alloc::vec![0; len];
        let mut filled = 0;
        while filled < len {
            let read = file.read(&mut bytes[filled..]).await?;
            if read == 0 {
                return Err(FsError::Io);
            }
            filled = filled.checked_add(read).ok_or(FsError::Io)?;
        }
        Ok(bytes)
    }

    /// Returns a file's byte length.
    pub async fn len(&self, path: &str) -> Result<u64, FsError> {
        Ok(self.metadata(path).await?.len())
    }

    /// Returns whether a path exists.
    pub async fn exists(&self, path: &str) -> Result<bool, FsError> {
        match self.metadata(path).await {
            Ok(_metadata) => Ok(true),
            Err(FsError::NotFound) => Ok(false),
            Err(error) => Err(error),
        }
    }

    /// Lists immediate entry names.
    pub async fn list_dir(&self, path: &str) -> Result<Vec<String>, FsError> {
        self.read_dir(path)
            .await?
            .map(|entry| entry.map(|entry| String::from(entry.file_name())))
            .collect()
    }

    /// Removes a file or empty directory, succeeding when it is absent.
    pub async fn remove(&self, path: &str) -> Result<(), FsError> {
        match self.metadata(path).await {
            Ok(metadata) if metadata.is_dir() => self.remove_dir(path).await,
            Ok(_metadata) => self.remove_file(path).await,
            Err(FsError::NotFound) => Ok(()),
            Err(error) => Err(error),
        }
    }

    /// Returns metadata for one path.
    pub async fn metadata(&self, path: &str) -> Result<Metadata, FsError> {
        match self.locate(path)? {
            Location::Mounted { path, root, .. } => match self.vfs.metadata(&path).await {
                Err(FsError::NotFound) if root => Ok(DIRECTORY),
                result => result,
            },
            Location::Above(_) => Ok(DIRECTORY),
        }
    }

    /// Lists immediate directory children.
    pub async fn read_dir(&self, path: &str) -> Result<ReadDir, FsError> {
        match self.locate(path)? {
            Location::Mounted { path, root, .. } => match self.vfs.read_dir(&path).await {
                Err(FsError::NotFound) if root => Ok(ReadDir::new(Vec::new())),
                result => result,
            },
            Location::Above(path) => Ok(ReadDir::new(self.mount_point_entries(&path))),
        }
    }

    /// Creates a directory and missing ancestors.
    pub async fn create_dir_all(&self, path: &str) -> Result<(), FsError> {
        self.vfs.create_dir_all(&self.writable_path(path)?).await
    }

    /// Removes one regular file.
    pub async fn remove_file(&self, path: &str) -> Result<(), FsError> {
        let path = self.entry_path(path, FsError::IsDirectory)?;
        self.vfs.remove_file(&path).await
    }

    /// Removes one empty directory.
    pub async fn remove_dir(&self, path: &str) -> Result<(), FsError> {
        let path = self.entry_path(path, FsError::PermissionDenied)?;
        self.vfs.remove_dir(&path).await
    }

    /// Renames a path without leaving this scoped root.
    pub async fn rename(&self, from: &str, to: &str) -> Result<(), FsError> {
        let from = self.entry_path(from, FsError::PermissionDenied)?;
        let to = self.entry_path(to, FsError::PermissionDenied)?;
        self.vfs.rename(&from, &to).await
    }
}

const DIRECTORY: Metadata = Metadata::new(FileType::Directory, 0);

/// Where a view path lands.
enum Location {
    /// Inside a mount; `root` marks the mount point itself.
    Mounted {
        path: String,
        root: bool,
        read_only: bool,
    },
    /// Above one or more mount points, outside every mount.
    Above(String),
}

#[derive(Clone)]
pub(crate) struct ScopedMount {
    pub(crate) point: Cow<'static, str>,
    source_root: Cow<'static, str>,
    read_only: bool,
}

impl ScopedMount {
    pub(crate) const fn new(
        point: Cow<'static, str>,
        source_root: Cow<'static, str>,
        options: MountOptions,
    ) -> Self {
        Self {
            point,
            source_root,
            read_only: options.is_read_only(),
        }
    }
}
