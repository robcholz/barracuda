use alloc::string::String;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicUsize, Ordering};

use embedded_io_async::{Read, Seek, Write};

use crate::path::normalize;
use crate::{File, FsError, Metadata, OpenOptions, ReadDir, SeekFrom, Vfs};

static TEMP_FILE_SEQUENCE: AtomicUsize = AtomicUsize::new(0);

/// A cloneable filesystem view rooted beneath one path in an existing VFS.
///
/// It exposes file operations only; mount-table ownership remains with the
/// [`Vfs`] that created the view.
#[derive(Clone)]
pub struct ScopedVfs {
    vfs: Vfs,
    root: String,
}

impl ScopedVfs {
    pub(crate) fn new(vfs: Vfs, root: String) -> Self {
        Self { vfs, root }
    }

    async fn ensure_root(&self) -> Result<(), FsError> {
        self.vfs.create_dir_all(&self.root).await
    }

    fn path(&self, path: &str) -> Result<String, FsError> {
        let path = if path.starts_with('/') {
            normalize(path)?
        } else {
            let mut relative = String::with_capacity(path.len().saturating_add(1));
            relative.push('/');
            relative.push_str(path);
            normalize(&relative)?
        };
        if path == "/" {
            return Ok(self.root.clone());
        }
        let mut scoped = String::with_capacity(self.root.len().saturating_add(path.len()));
        scoped.push_str(&self.root);
        scoped.push_str(&path);
        Ok(scoped)
    }

    async fn prepare_file_path(&self, path: &str) -> Result<String, FsError> {
        self.ensure_root().await?;
        let path = self.path(path)?;
        if let Some((parent, _name)) = path.rsplit_once('/') {
            let parent = if parent.is_empty() { "/" } else { parent };
            self.vfs.create_dir_all(parent).await?;
        }
        Ok(path)
    }

    /// Opens an existing file for reading.
    pub async fn open(&self, path: &str) -> Result<File, FsError> {
        self.ensure_root().await?;
        self.vfs.open(&self.path(path)?).await
    }

    /// Creates or truncates a file for writing.
    pub async fn create(&self, path: &str) -> Result<File, FsError> {
        let path = self.prepare_file_path(path).await?;
        self.vfs.create(&path).await
    }

    /// Opens a file with explicit options.
    pub async fn open_with(&self, path: &str, options: &OpenOptions) -> Result<File, FsError> {
        self.ensure_root().await?;
        self.vfs.open_with(&self.path(path)?, options).await
    }

    /// Reads an entire file.
    pub async fn read(&self, path: &str) -> Result<Vec<u8>, FsError> {
        self.ensure_root().await?;
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
        let temporary = alloc::format!("{target}.barracuda-tmp-{sequence}");
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
        self.ensure_root().await?;
        self.vfs.metadata(&self.path(path)?).await
    }

    /// Lists immediate directory children.
    pub async fn read_dir(&self, path: &str) -> Result<ReadDir, FsError> {
        self.ensure_root().await?;
        self.vfs.read_dir(&self.path(path)?).await
    }

    /// Creates a directory and missing ancestors.
    pub async fn create_dir_all(&self, path: &str) -> Result<(), FsError> {
        self.ensure_root().await?;
        self.vfs.create_dir_all(&self.path(path)?).await
    }

    /// Removes one regular file.
    pub async fn remove_file(&self, path: &str) -> Result<(), FsError> {
        self.ensure_root().await?;
        self.vfs.remove_file(&self.path(path)?).await
    }

    /// Removes one empty directory.
    pub async fn remove_dir(&self, path: &str) -> Result<(), FsError> {
        self.ensure_root().await?;
        self.vfs.remove_dir(&self.path(path)?).await
    }

    /// Renames a path without leaving this scoped root.
    pub async fn rename(&self, from: &str, to: &str) -> Result<(), FsError> {
        self.ensure_root().await?;
        self.vfs.rename(&self.path(from)?, &self.path(to)?).await
    }
}
