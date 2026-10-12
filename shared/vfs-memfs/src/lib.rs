#![no_std]

//! In-memory backend for `barracuda-vfs`.
//!
//! The directory tree and every file's control state live on the global heap.
//! File contents are split into 4 KiB pages allocated from bulk memory,
//! so a file grows by adding pages and reads and writes address pages by
//! offset without copying the whole file.
//!
//! Bulk memory is the only capacity limit. A write that needs pages the bulk
//! allocator cannot supply fails with [`FsError::NoSpace`] and changes nothing.
//!
//! An open file holds its file, not its path: it keeps reading and writing the
//! same contents after the path is removed or renamed.

extern crate alloc;

use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;

use barracuda_bulk_memory::BulkVec;
use barracuda_vfs::{
    Backend, BackendFile, BackendFuture, DirEntry, FileType, FsError, Metadata, OpenOptions,
    SeekFrom, VfsBackend,
};
use portable_atomic_util::Arc;
use spin::Mutex;

/// Bytes held by every file page except a file's last.
const PAGE_SIZE: usize = 4096;

/// A cloneable in-memory filesystem whose file pages live in bulk memory.
///
/// Clones share one tree.
#[derive(Clone)]
pub struct MemFs {
    root: Arc<Mutex<Node>>,
}

impl MemFs {
    /// Creates an empty filesystem containing only its root directory.
    pub fn new() -> Self {
        Self {
            root: Arc::new(Mutex::new(Node::Directory(Directory::new()))),
        }
    }

    /// Erases this filesystem for mounting into a VFS namespace.
    pub fn into_backend(self) -> Backend {
        Backend::new(self)
    }

    /// Recursively creates a directory directly in this backend.
    ///
    /// # Errors
    ///
    /// Returns [`FsError::NotDirectory`] when the path or one of its ancestors
    /// is a file.
    pub fn create_dir_all(&self, path: &str) -> Result<(), FsError> {
        let mut root = self.root.lock();
        let mut node = &mut *root;
        for name in components(path) {
            let Node::Directory(children) = node else {
                return Err(FsError::NotDirectory);
            };
            if !children.contains_key(name) {
                children.insert(String::from(name), Node::Directory(Directory::new()));
            }
            node = children.get_mut(name).ok_or(FsError::Io)?;
        }
        match node {
            Node::Directory(_) => Ok(()),
            Node::File(_) => Err(FsError::NotDirectory),
        }
    }

    /// Replaces one file directly in this backend, creating it when absent.
    ///
    /// Handles already open on the file observe the new contents.
    ///
    /// # Errors
    ///
    /// Returns [`FsError::NotFound`] when the parent directory is missing,
    /// [`FsError::IsDirectory`] when the path is a directory, and
    /// [`FsError::NoSpace`] when bulk memory cannot hold the contents, in
    /// which case the existing file is unchanged.
    pub fn write_file(&self, path: &str, bytes: &[u8]) -> Result<(), FsError> {
        let path = components(path);
        let (name, parent) = path.split_last().ok_or(FsError::IsDirectory)?;
        let mut contents = FileData::new();
        contents.write_at(0, bytes)?;

        let mut root = self.root.lock();
        let directory = directory_mut(&mut root, parent)?;
        match directory.get(*name) {
            Some(Node::Directory(_)) => Err(FsError::IsDirectory),
            Some(Node::File(file)) => {
                *file.lock() = contents;
                Ok(())
            }
            None => {
                directory.insert(
                    String::from(*name),
                    Node::File(Arc::new(Mutex::new(contents))),
                );
                Ok(())
            }
        }
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
            let path = components(path);
            let mut root = self.root.lock();
            let file = match find(&root, &path) {
                Ok(_) if options.is_create_new() => return Err(FsError::AlreadyExists),
                Ok(Node::Directory(_)) => return Err(FsError::IsDirectory),
                Ok(Node::File(file)) => {
                    if options.should_truncate() {
                        file.lock().clear();
                    }
                    file.clone()
                }
                Err(_) if !options.should_create() => return Err(FsError::NotFound),
                Err(_) => {
                    let (name, parent) = path.split_last().ok_or(FsError::IsDirectory)?;
                    let file = Arc::new(Mutex::new(FileData::new()));
                    directory_mut(&mut root, parent)?
                        .insert(String::from(*name), Node::File(file.clone()));
                    file
                }
            };
            drop(root);

            let cursor = if options.is_append() {
                u64::try_from(file.lock().len()).map_err(|_| FsError::Io)?
            } else {
                0
            };
            Ok(Box::new(MemFile {
                file,
                cursor,
                readable: options.can_read(),
                writable: options.can_write(),
                append: options.is_append(),
            }) as Box<dyn BackendFile>)
        })
    }

    fn metadata<'a>(&'a self, path: &'a str) -> BackendFuture<'a, Result<Metadata, FsError>> {
        Box::pin(async move { find(&self.root.lock(), &components(path))?.metadata() })
    }

    fn read_dir<'a>(&'a self, path: &'a str) -> BackendFuture<'a, Result<Vec<DirEntry>, FsError>> {
        Box::pin(async move {
            let root = self.root.lock();
            let Node::Directory(children) = find(&root, &components(path))? else {
                return Err(FsError::NotDirectory);
            };
            children
                .iter()
                .map(|(name, node)| Ok(DirEntry::new(name.as_str(), node.metadata()?)))
                .collect()
        })
    }

    fn create_dir_all<'a>(&'a self, path: &'a str) -> BackendFuture<'a, Result<(), FsError>> {
        Box::pin(async move { MemFs::create_dir_all(self, path) })
    }

    fn remove_file<'a>(&'a self, path: &'a str) -> BackendFuture<'a, Result<(), FsError>> {
        Box::pin(async move {
            let path = components(path);
            let (name, parent) = path.split_last().ok_or(FsError::IsDirectory)?;
            let mut root = self.root.lock();
            let directory = directory_mut(&mut root, parent)?;
            match directory.get(*name) {
                None => Err(FsError::NotFound),
                Some(Node::Directory(_)) => Err(FsError::IsDirectory),
                Some(Node::File(_)) => {
                    directory.remove(*name);
                    Ok(())
                }
            }
        })
    }

    fn remove_dir<'a>(&'a self, path: &'a str) -> BackendFuture<'a, Result<(), FsError>> {
        Box::pin(async move {
            let path = components(path);
            let (name, parent) = path.split_last().ok_or(FsError::PermissionDenied)?;
            let mut root = self.root.lock();
            let directory = directory_mut(&mut root, parent)?;
            match directory.get(*name) {
                None => Err(FsError::NotFound),
                Some(Node::File(_)) => Err(FsError::NotDirectory),
                Some(Node::Directory(children)) if !children.is_empty() => {
                    Err(FsError::DirectoryNotEmpty)
                }
                Some(Node::Directory(_)) => {
                    directory.remove(*name);
                    Ok(())
                }
            }
        })
    }

    /// Moves an entry, replacing whatever the destination names.
    ///
    /// An existing destination file or directory, empty or not, is dropped.
    /// Moving a directory into its own descendant fails with
    /// [`FsError::InvalidInput`].
    fn rename<'a>(&'a self, from: &'a str, to: &'a str) -> BackendFuture<'a, Result<(), FsError>> {
        Box::pin(async move {
            let from = components(from);
            let to = components(to);
            let (Some((from_name, from_parent)), Some((to_name, to_parent))) =
                (from.split_last(), to.split_last())
            else {
                return Err(FsError::PermissionDenied);
            };
            let mut root = self.root.lock();
            find(&root, &from)?;
            if from == to {
                return Ok(());
            }
            if to.starts_with(&from) {
                return Err(FsError::InvalidInput);
            }
            if let Node::File(_) = find(&root, to_parent)? {
                return Err(FsError::NotDirectory);
            }

            let node = directory_mut(&mut root, from_parent)?
                .remove(*from_name)
                .ok_or(FsError::NotFound)?;
            match directory_mut(&mut root, to_parent) {
                Ok(directory) => {
                    directory.insert(String::from(*to_name), node);
                    Ok(())
                }
                Err(error) => {
                    if let Ok(directory) = directory_mut(&mut root, from_parent) {
                        directory.insert(String::from(*from_name), node);
                    }
                    Err(error)
                }
            }
        })
    }
}

type Directory = BTreeMap<String, Node>;

/// One file's contents, shared by its directory entry and its open handles.
type SharedFile = Arc<Mutex<FileData>>;

enum Node {
    Directory(Directory),
    File(SharedFile),
}

impl Node {
    fn metadata(&self) -> Result<Metadata, FsError> {
        Ok(match self {
            Self::Directory(_) => Metadata::new(FileType::Directory, 0),
            Self::File(file) => Metadata::new(
                FileType::File,
                u64::try_from(file.lock().len()).map_err(|_| FsError::Io)?,
            ),
        })
    }
}

/// Splits a path into names, resolving `.` and `..` from the root.
fn components(path: &str) -> Vec<&str> {
    let mut names = Vec::new();
    for name in path.split('/') {
        match name {
            "" | "." => {}
            ".." => {
                names.pop();
            }
            name => names.push(name),
        }
    }
    names
}

fn find<'a>(root: &'a Node, path: &[&str]) -> Result<&'a Node, FsError> {
    path.iter().try_fold(root, |node, name| match node {
        Node::Directory(children) => children.get(*name).ok_or(FsError::NotFound),
        Node::File(_) => Err(FsError::NotDirectory),
    })
}

fn directory_mut<'a>(root: &'a mut Node, path: &[&str]) -> Result<&'a mut Directory, FsError> {
    let node = path.iter().try_fold(root, |node, name| match node {
        Node::Directory(children) => children.get_mut(*name).ok_or(FsError::NotFound),
        Node::File(_) => Err(FsError::NotDirectory),
    })?;
    match node {
        Node::Directory(children) => Ok(children),
        Node::File(_) => Err(FsError::NotDirectory),
    }
}

/// File contents as bulk-memory pages.
///
/// Every page except the last holds exactly [`PAGE_SIZE`] bytes, and the last
/// holds at least one byte, so the length and the page of any offset follow
/// from the page count alone.
struct FileData {
    pages: Vec<BulkVec<u8>>,
}

impl FileData {
    const fn new() -> Self {
        Self { pages: Vec::new() }
    }

    fn len(&self) -> usize {
        self.pages.last().map_or(0, |last| {
            (self.pages.len() - 1)
                .saturating_mul(PAGE_SIZE)
                .saturating_add(last.len())
        })
    }

    /// Drops every page.
    fn clear(&mut self) {
        self.pages = Vec::new();
    }

    /// Copies bytes from `offset` into `buffer`, returning how many were
    /// available.
    fn read_at(&self, offset: usize, buffer: &mut [u8]) -> Result<usize, FsError> {
        let amount = buffer.len().min(self.len().saturating_sub(offset));
        let mut copied = 0;
        for (page, range) in spans(offset, amount) {
            let source = self
                .pages
                .get(page)
                .and_then(|page| page.get(range.clone()))
                .ok_or(FsError::Io)?;
            let end = copied + range.len();
            buffer
                .get_mut(copied..end)
                .ok_or(FsError::Io)?
                .copy_from_slice(source);
            copied = end;
        }
        Ok(amount)
    }

    /// Writes all of `bytes` at `offset`, zero-filling any gap after the
    /// current end.
    ///
    /// Either the whole write is applied or, on [`FsError::NoSpace`], the
    /// contents and length are unchanged.
    fn write_at(&mut self, offset: usize, bytes: &[u8]) -> Result<usize, FsError> {
        if bytes.is_empty() {
            return Ok(0);
        }
        let end = offset
            .checked_add(bytes.len())
            .ok_or(FsError::InvalidInput)?;
        self.grow(end)?;
        let mut copied = 0;
        for (page, range) in spans(offset, bytes.len()) {
            let next = copied + range.len();
            let source = bytes.get(copied..next).ok_or(FsError::Io)?;
            self.pages
                .get_mut(page)
                .and_then(|page| page.get_mut(range))
                .ok_or(FsError::Io)?
                .copy_from_slice(source);
            copied = next;
        }
        Ok(bytes.len())
    }

    /// Zero-extends the contents to at least `len` bytes.
    ///
    /// Every allocation happens before any length changes, so a failure
    /// leaves the contents as they were.
    fn grow(&mut self, len: usize) -> Result<(), FsError> {
        if len <= self.len() {
            return Ok(());
        }
        let first_new = self.pages.len();
        let count = len.div_ceil(PAGE_SIZE);
        self.pages
            .try_reserve(count - first_new)
            .map_err(|_| FsError::NoSpace)?;

        if let Some(last) = self.pages.last_mut() {
            let needed = page_len(len, first_new - 1);
            if last.capacity() < needed {
                // Grow a partial last page geometrically so small appends do
                // not reallocate it on every write.
                let target = needed.max(last.capacity().saturating_mul(2)).min(PAGE_SIZE);
                last.try_reserve_exact(target - last.len())
                    .map_err(|_| FsError::NoSpace)?;
            }
        }
        let mut fresh = Vec::new();
        fresh
            .try_reserve_exact(count - first_new)
            .map_err(|_| FsError::NoSpace)?;
        for page in first_new..count {
            fresh.push(
                BulkVec::try_with_capacity(page_len(len, page)).map_err(|_| FsError::NoSpace)?,
            );
        }

        // Every page now has the capacity it needs: nothing below allocates.
        self.pages.extend(fresh);
        for (index, page) in self
            .pages
            .iter_mut()
            .enumerate()
            .skip(first_new.saturating_sub(1))
        {
            page.resize(page_len(len, index), 0);
        }
        Ok(())
    }
}

/// Bytes page `page` holds in a file of `len` bytes.
fn page_len(len: usize, page: usize) -> usize {
    len.saturating_sub(page.saturating_mul(PAGE_SIZE))
        .min(PAGE_SIZE)
}

/// Splits `len` bytes from `offset` into each page's index and byte range.
fn spans(offset: usize, len: usize) -> impl Iterator<Item = (usize, core::ops::Range<usize>)> {
    let end = offset.saturating_add(len);
    let mut position = offset;
    core::iter::from_fn(move || {
        if position >= end {
            return None;
        }
        let page = position / PAGE_SIZE;
        let start = position % PAGE_SIZE;
        let amount = (PAGE_SIZE - start).min(end - position);
        position += amount;
        Some((page, start..start + amount))
    })
}

struct MemFile {
    file: SharedFile,
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
            let offset = usize::try_from(self.cursor).map_err(|_| FsError::InvalidInput)?;
            let amount = self.file.lock().read_at(offset, buffer)?;
            self.cursor = self
                .cursor
                .checked_add(u64::try_from(amount).map_err(|_| FsError::Io)?)
                .ok_or(FsError::Io)?;
            Ok(amount)
        })
    }

    /// Writes the whole buffer or, on [`FsError::NoSpace`], nothing.
    fn write<'a>(&'a mut self, buffer: &'a [u8]) -> BackendFuture<'a, Result<usize, FsError>> {
        Box::pin(async move {
            if !self.writable {
                return Err(FsError::PermissionDenied);
            }
            let mut file = self.file.lock();
            let offset = if self.append {
                file.len()
            } else {
                usize::try_from(self.cursor).map_err(|_| FsError::InvalidInput)?
            };
            let written = file.write_at(offset, buffer)?;
            drop(file);
            self.cursor = u64::try_from(offset + written).map_err(|_| FsError::Io)?;
            Ok(written)
        })
    }

    fn flush(&mut self) -> BackendFuture<'_, Result<(), FsError>> {
        Box::pin(async { Ok(()) })
    }

    fn seek(&mut self, position: SeekFrom) -> BackendFuture<'_, Result<u64, FsError>> {
        Box::pin(async move {
            let next = match position {
                SeekFrom::Start(offset) => Some(offset),
                SeekFrom::End(offset) => u64::try_from(self.file.lock().len())
                    .map_err(|_| FsError::Io)?
                    .checked_add_signed(offset),
                SeekFrom::Current(offset) => self.cursor.checked_add_signed(offset),
            };
            self.cursor = next.ok_or(FsError::InvalidInput)?;
            Ok(self.cursor)
        })
    }
}
