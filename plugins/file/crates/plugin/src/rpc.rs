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

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use alloc::boxed::Box;
    use barracuda_event_router::{RpcLaneStorage, RpcRegistry};
    use barracuda_platform_test::memory_vfs;
    use futures_lite::future::block_on;

    fn registry() -> (RpcRegistry<4, 1024, 4>, Rc<FileSystem>) {
        let lanes = Box::leak(Box::new(RpcLaneStorage::<4, 1024, 4>::new()));
        let registry = RpcRegistry::new(lanes);
        let filesystem = Rc::new(FileSystem::new(
            block_on(memory_vfs()).expect("memory filesystem mounts"),
        ));
        registry
            .register::<FileRead, _>(read_handler(Rc::clone(&filesystem)))
            .expect("register read endpoint");
        registry
            .register::<FileWrite, _>(write_handler(Rc::clone(&filesystem)))
            .expect("register write endpoint");
        (registry, filesystem)
    }

    #[test]
    fn file_rpc_round_trips_bytes_and_maps_storage_failures() {
        block_on(async {
            let (registry, filesystem) = registry();
            let client = registry.client();

            let written = client
                .call::<FileWrite>(FileWriteRequest::new("/nested/message.bin", b"hello").unwrap())
                .expect("start write")
                .await
                .expect("write transport")
                .expect("write succeeds");
            assert_eq!(written.view(), Ok(&()));
            drop(written);

            let read = client
                .call::<FileRead>(FileReadRequest::new("/nested/message.bin").unwrap())
                .expect("start read")
                .await
                .expect("read transport")
                .expect("read succeeds");
            assert_eq!(read.view().unwrap().as_bytes(), Ok(b"hello".as_slice()));
            drop(read);

            let missing = client
                .call::<FileRead>(FileReadRequest::new("/missing").unwrap())
                .expect("start missing read")
                .await
                .expect("missing read transport")
                .expect_err("missing file is a method error");
            assert_eq!(missing.view(), Ok(&FileRpcError::NotFound));
            drop(missing);

            filesystem
                .write("/oversized", &[7; CONTENT_CAPACITY + 1])
                .await
                .expect("seed oversized file");
            let oversized = client
                .call::<FileRead>(FileReadRequest::new("/oversized").unwrap())
                .expect("start oversized read")
                .await
                .expect("oversized read transport")
                .expect_err("oversized file is a method error");
            assert_eq!(oversized.view(), Ok(&FileRpcError::TooLarge));
        });
    }

    #[test]
    fn bounded_rpc_values_reject_ambiguous_or_oversized_inputs() {
        assert_eq!(FilePath::new(""), Err(FileRpcError::InvalidRequest));
        assert_eq!(FilePath::new("a\0b"), Err(FileRpcError::InvalidRequest));
        assert_eq!(
            FilePath::new(&"p".repeat(PATH_CAPACITY)),
            Err(FileRpcError::InvalidRequest)
        );
        assert_eq!(
            FileBytes::new(&[0; CONTENT_CAPACITY + 1]),
            Err(FileRpcError::TooLarge)
        );

        let path = FilePath::new("/valid").unwrap();
        assert_eq!(path.as_str(), Ok("/valid"));
        assert_eq!(serde_json::to_string(&path).unwrap(), r#""/valid""#);
        assert_eq!(
            serde_json::from_str::<FilePath>(r#""/decoded""#)
                .unwrap()
                .as_str(),
            Ok("/decoded")
        );
    }

    #[test]
    fn filesystem_errors_have_stable_rpc_categories() {
        assert_eq!(
            map_fs(barracuda_vfs::FsError::PermissionDenied),
            FileRpcError::PermissionDenied
        );
        assert_eq!(
            map_fs(barracuda_vfs::FsError::ReadOnly),
            FileRpcError::PermissionDenied
        );
        assert_eq!(
            map_fs(barracuda_vfs::FsError::NotFound),
            FileRpcError::NotFound
        );
        assert_eq!(
            map_fs(barracuda_vfs::FsError::InvalidPath),
            FileRpcError::Io
        );
    }
}
