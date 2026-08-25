use core::fmt;

/// A portable virtual-filesystem failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum FsError {
    /// The requested path does not exist.
    NotFound,
    /// The requested path already exists.
    AlreadyExists,
    /// Access was denied by the backend.
    PermissionDenied,
    /// The selected mount is read-only.
    ReadOnly,
    /// The supplied path is not an absolute, contained VFS path.
    InvalidPath,
    /// No mounted filesystem owns the requested path.
    NotMounted,
    /// A filesystem is already mounted at the requested mount point.
    MountConflict,
    /// An operation attempted to cross a mount boundary.
    CrossMount,
    /// The mount still owns open file handles.
    Busy,
    /// A file operation addressed a directory.
    IsDirectory,
    /// A directory operation addressed a non-directory.
    NotDirectory,
    /// A directory could not be removed because it is not empty.
    DirectoryNotEmpty,
    /// The backend does not support this operation.
    Unsupported,
    /// The supplied open options or operation arguments are inconsistent.
    InvalidInput,
    /// The backend reported an I/O failure without a more precise mapping.
    Io,
}

impl fmt::Display for FsError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::NotFound => "path not found",
            Self::AlreadyExists => "path already exists",
            Self::PermissionDenied => "permission denied",
            Self::ReadOnly => "filesystem is read-only",
            Self::InvalidPath => "invalid VFS path",
            Self::NotMounted => "path is not mounted",
            Self::MountConflict => "mount point is already occupied",
            Self::CrossMount => "operation crosses a mount boundary",
            Self::Busy => "filesystem is busy",
            Self::IsDirectory => "path is a directory",
            Self::NotDirectory => "path is not a directory",
            Self::DirectoryNotEmpty => "directory is not empty",
            Self::Unsupported => "operation is not supported",
            Self::InvalidInput => "invalid filesystem input",
            Self::Io => "filesystem I/O failure",
        })
    }
}

impl core::error::Error for FsError {}

impl embedded_io::Error for FsError {
    fn kind(&self) -> embedded_io::ErrorKind {
        match self {
            Self::NotFound => embedded_io::ErrorKind::NotFound,
            Self::AlreadyExists => embedded_io::ErrorKind::AlreadyExists,
            Self::PermissionDenied | Self::ReadOnly => embedded_io::ErrorKind::PermissionDenied,
            Self::InvalidPath | Self::InvalidInput => embedded_io::ErrorKind::InvalidInput,
            _ => embedded_io::ErrorKind::Other,
        }
    }
}
