use alloc::rc::Rc;

use barracuda_event_router::{RpcFrame, RpcHandler, RpcMethod, Unary, rpc_dynamic, rpc_message};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use zerocopy::{Immutable, IntoBytes, KnownLayout, TryFromBytes};

use crate::FileSystem;

const PATH_CAPACITY: usize = 256;
const CONTENT_CAPACITY: usize = 240;

/// A bounded UTF-8 filesystem path.
#[repr(transparent)]
#[derive(Clone, Copy, Debug, Eq, Immutable, IntoBytes, KnownLayout, PartialEq, TryFromBytes)]
pub struct FilePath([u8; PATH_CAPACITY]);

impl FilePath {
    /// Creates a path accepted by the dynamic RPC contract.
    pub fn new(value: &str) -> Result<Self, FileRpcError> {
        if value.is_empty() || value.as_bytes().contains(&0) || value.len() >= PATH_CAPACITY {
            return Err(FileRpcError::InvalidRequest);
        }
        let mut bytes = [0; PATH_CAPACITY];
        bytes
            .get_mut(..value.len())
            .ok_or(FileRpcError::InvalidRequest)?
            .copy_from_slice(value.as_bytes());
        Ok(Self(bytes))
    }

    /// Returns the path text.
    pub fn as_str(&self) -> Result<&str, FileRpcError> {
        let end = self
            .0
            .iter()
            .position(|byte| *byte == 0)
            .unwrap_or(PATH_CAPACITY);
        core::str::from_utf8(self.0.get(..end).ok_or(FileRpcError::InvalidRequest)?)
            .map_err(|_| FileRpcError::InvalidRequest)
    }
}

impl Serialize for FilePath {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str().map_err(serde::ser::Error::custom)?)
    }
}
impl<'de> Deserialize<'de> for FilePath {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = alloc::string::String::deserialize(deserializer)?;
        Self::new(&value).map_err(serde::de::Error::custom)
    }
}

/// Bounded opaque file contents.
#[repr(C)]
#[rpc_message]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FileBytes {
    length: u16,
    #[serde(with = "serde_big_array::BigArray")]
    bytes: [u8; CONTENT_CAPACITY],
}

impl FileBytes {
    /// Copies bytes into one RPC frame.
    pub fn new(value: &[u8]) -> Result<Self, FileRpcError> {
        let length = u16::try_from(value.len()).map_err(|_| FileRpcError::TooLarge)?;
        if value.len() > CONTENT_CAPACITY {
            return Err(FileRpcError::TooLarge);
        }
        let mut bytes = [0; CONTENT_CAPACITY];
        bytes
            .get_mut(..value.len())
            .ok_or(FileRpcError::TooLarge)?
            .copy_from_slice(value);
        Ok(Self { length, bytes })
    }

    /// Returns the carried bytes.
    pub fn as_bytes(&self) -> Result<&[u8], FileRpcError> {
        self.bytes
            .get(..usize::from(self.length))
            .ok_or(FileRpcError::InvalidRequest)
    }
}

/// Request for `file.read`.
#[repr(C)]
#[rpc_message]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FileReadRequest {
    path: FilePath,
}

impl FileReadRequest {
    /// Creates a read request.
    pub fn new(path: &str) -> Result<Self, FileRpcError> {
        Ok(Self {
            path: FilePath::new(path)?,
        })
    }
}

/// Request for `file.write`.
#[repr(C)]
#[rpc_message]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FileWriteRequest {
    path: FilePath,
    content: FileBytes,
}

impl FileWriteRequest {
    /// Creates a write request.
    pub fn new(path: &str, content: &[u8]) -> Result<Self, FileRpcError> {
        Ok(Self {
            path: FilePath::new(path)?,
            content: FileBytes::new(content)?,
        })
    }
}

/// Filesystem operation rejection.
#[repr(u8)]
#[derive(
    Clone,
    Copy,
    Debug,
    Deserialize,
    Eq,
    Immutable,
    IntoBytes,
    KnownLayout,
    PartialEq,
    Serialize,
    TryFromBytes,
)]
#[serde(rename_all = "snake_case")]
pub enum FileRpcError {
    /// The request encoding or path is invalid.
    InvalidRequest,
    /// The file was not found.
    NotFound,
    /// Access was denied or the mount is read-only.
    PermissionDenied,
    /// File contents exceed one RPC frame.
    TooLarge,
    /// Another filesystem operation failed.
    Io,
}

impl core::fmt::Display for FileRpcError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(match self {
            Self::InvalidRequest => "invalid request",
            Self::NotFound => "not found",
            Self::PermissionDenied => "permission denied",
            Self::TooLarge => "file too large",
            Self::Io => "filesystem I/O failure",
        })
    }
}

fn map_fs(error: barracuda_vfs::FsError) -> FileRpcError {
    match error {
        barracuda_vfs::FsError::NotFound => FileRpcError::NotFound,
        barracuda_vfs::FsError::PermissionDenied | barracuda_vfs::FsError::ReadOnly => {
            FileRpcError::PermissionDenied
        }
        _ => FileRpcError::Io,
    }
}

/// Reads a bounded file from the System filesystem.
pub struct FileRead;
#[rpc_dynamic]
impl RpcMethod for FileRead {
    const ADDRESS: &'static str = "file.read";
    type Request = FileReadRequest;
    type Response = FileBytes;
    type Error = FileRpcError;
    type Input = Unary;
    type Output = Unary;
}

/// Writes a bounded file in the System filesystem.
pub struct FileWrite;
#[rpc_dynamic]
impl RpcMethod for FileWrite {
    const ADDRESS: &'static str = "file.write";
    type Request = FileWriteRequest;
    type Response = ();
    type Error = FileRpcError;
    type Input = Unary;
    type Output = Unary;
}

pub(crate) fn read_handler(filesystem: Rc<FileSystem>) -> impl RpcHandler<FileRead> {
    move |_context, request: RpcFrame<FileReadRequest>| {
        let filesystem = Rc::clone(&filesystem);
        async move {
            let request = request.view()?;
            let path = match request.path.as_str() {
                Ok(path) => path,
                Err(error) => return Ok(Err(error)),
            };
            match filesystem.read(path).await {
                Ok(bytes) => Ok(FileBytes::new(&bytes)),
                Err(error) => Ok(Err(map_fs(error))),
            }
        }
    }
}
pub(crate) fn write_handler(filesystem: Rc<FileSystem>) -> impl RpcHandler<FileWrite> {
    move |_context, request: RpcFrame<FileWriteRequest>| {
        let filesystem = Rc::clone(&filesystem);
        async move {
            let request = request.view()?;
            let path = match request.path.as_str() {
                Ok(path) => path,
                Err(error) => return Ok(Err(error)),
            };
            let content = match request.content.as_bytes() {
                Ok(content) => content,
                Err(error) => return Ok(Err(error)),
            };
            match filesystem.write(path, content).await {
                Ok(()) => Ok(Ok(())),
                Err(error) => Ok(Err(map_fs(error))),
            }
        }
    }
}
