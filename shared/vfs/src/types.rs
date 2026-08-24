use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use crate::FsError;

/// Options used when opening a file.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct OpenOptions {
    pub(crate) read: bool,
    pub(crate) write: bool,
    pub(crate) append: bool,
    pub(crate) truncate: bool,
    pub(crate) create: bool,
    pub(crate) create_new: bool,
}

impl OpenOptions {
    /// Creates an option set with every capability disabled.
    pub const fn new() -> Self {
        Self {
            read: false,
            write: false,
            append: false,
            truncate: false,
            create: false,
            create_new: false,
        }
    }

    /// Enables or disables reading.
    pub fn read(&mut self, enabled: bool) -> &mut Self {
        self.read = enabled;
        self
    }

    /// Enables or disables writing.
    pub fn write(&mut self, enabled: bool) -> &mut Self {
        self.write = enabled;
        self
    }

    /// Enables or disables append mode.
    pub fn append(&mut self, enabled: bool) -> &mut Self {
        self.append = enabled;
        self
    }

    /// Enables or disables truncating an existing file.
    pub fn truncate(&mut self, enabled: bool) -> &mut Self {
        self.truncate = enabled;
        self
    }

    /// Enables or disables creating a missing file.
    pub fn create(&mut self, enabled: bool) -> &mut Self {
        self.create = enabled;
        self
    }

    /// Enables exclusive file creation.
    pub fn create_new(&mut self, enabled: bool) -> &mut Self {
        self.create_new = enabled;
        self
    }

    /// Returns whether the option set can modify storage.
    pub const fn mutates(self) -> bool {
        self.write || self.append || self.truncate || self.create || self.create_new
    }

    pub(crate) const fn validates(self) -> bool {
        (self.read || self.write || self.append)
            && (!self.truncate || self.write)
            && (!(self.create || self.create_new) || self.write || self.append)
    }

    pub(crate) const fn read_only() -> Self {
        Self {
            read: true,
            ..Self::new()
        }
    }

    pub(crate) const fn create_truncate() -> Self {
        Self {
            write: true,
            truncate: true,
            create: true,
            ..Self::new()
        }
    }

    /// Returns whether reading is enabled.
    pub const fn can_read(self) -> bool {
        self.read
    }

    /// Returns whether writing is enabled.
    pub const fn can_write(self) -> bool {
        self.write || self.append
    }

    /// Returns whether append mode is enabled.
    pub const fn is_append(self) -> bool {
        self.append
    }

    /// Returns whether truncation is enabled.
    pub const fn should_truncate(self) -> bool {
        self.truncate
    }

    /// Returns whether a missing file may be created.
    pub const fn should_create(self) -> bool {
        self.create || self.create_new
    }

    /// Returns whether creation must fail when the path exists.
    pub const fn is_create_new(self) -> bool {
        self.create_new
    }
}

/// Per-mount access policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MountOptions {
    read_only: bool,
}

impl MountOptions {
    /// Creates a writable mount policy.
    pub const fn read_write() -> Self {
        Self { read_only: false }
    }

    /// Creates a read-only mount policy.
    pub const fn read_only() -> Self {
        Self { read_only: true }
    }

    /// Returns whether writes are forbidden by the mount.
    pub const fn is_read_only(self) -> bool {
        self.read_only
    }
}

/// Portable entry type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileType {
    /// A regular byte file.
    File,
    /// A directory.
    Directory,
    /// A symbolic link or another backend-specific link.
    Symlink,
    /// A backend entry whose type is not representable by the common API.
    Other,
}

/// Portable file metadata.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Metadata {
    file_type: FileType,
    len: u64,
}

impl Metadata {
    /// Creates metadata for a backend entry.
    pub const fn new(file_type: FileType, len: u64) -> Self {
        Self { file_type, len }
    }

    /// Returns the entry type.
    pub const fn file_type(self) -> FileType {
        self.file_type
    }

    /// Returns the byte length, or zero for entries without byte contents.
    pub const fn len(self) -> u64 {
        self.len
    }

    /// Returns whether the byte length is zero.
    pub const fn is_empty(self) -> bool {
        self.len == 0
    }

    /// Returns whether this is a regular file.
    pub const fn is_file(self) -> bool {
        matches!(self.file_type, FileType::File)
    }

    /// Returns whether this is a directory.
    pub const fn is_dir(self) -> bool {
        matches!(self.file_type, FileType::Directory)
    }
}

/// One immediate child returned by [`ReadDir`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirEntry {
    name: String,
    metadata: Metadata,
}

impl DirEntry {
    /// Creates a directory entry.
    pub fn new(name: impl Into<String>, metadata: Metadata) -> Self {
        Self {
            name: name.into(),
            metadata,
        }
    }

    /// Returns the final path component of this entry.
    pub fn file_name(&self) -> &str {
        &self.name
    }

    /// Returns cached metadata supplied by the backend.
    pub const fn metadata(&self) -> Metadata {
        self.metadata
    }
}

/// Eager, allocation-backed directory iterator.
pub struct ReadDir {
    entries: vec::IntoIter<DirEntry>,
}

impl ReadDir {
    pub(crate) fn new(entries: Vec<DirEntry>) -> Self {
        Self {
            entries: entries.into_iter(),
        }
    }
}

impl Iterator for ReadDir {
    type Item = Result<DirEntry, FsError>;

    fn next(&mut self) -> Option<Self::Item> {
        self.entries.next().map(Ok)
    }
}
