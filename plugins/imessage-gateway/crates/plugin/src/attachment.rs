//! Files that users send through channels, and files the Gateway sends by
//! path.
//!
//! An inbound attachment is written to `/workspace/cache/gateway/<channel>/`
//! as `.<name>.part` and renamed into place once complete, so a reader never
//! sees half a file under its real name. Only the [`Attachment`] reference
//! travels on; the bytes stay in the file.

use alloc::boxed::Box;
use alloc::format;
use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec;
use core::cell::{Cell, OnceCell};
use core::fmt::Write as _;

use barracuda_imessage_gateway_runtime::{Attachment, GatewayOperationError, MediaOpener};
use barracuda_platform::Entropy as _;
use barracuda_plugin::api::SharedEntropy;
use barracuda_vfs::{File, FsError, OpenOptions, ScopedVfs};
use embedded_io_async::{Read as _, Write as _};
use futures_lite::{stream, StreamExt as _};
use gateway::{
    mime_extension, BinaryChunk, BinaryStream, MediaKind, MediaOpenFuture, MediaSource, StreamError,
};

use crate::http::{DownloadHead, DownloadSink};

/// Directory of every channel's inbound attachments in the Gateway's view.
pub const INBOUND_ROOT: &str = "/workspace/cache/gateway";

/// MIME type of an attachment whose type nobody named.
const UNKNOWN_MIME: &str = "application/octet-stream";

/// Longest directory name a channel gets below [`INBOUND_ROOT`].
const CHANNEL_SEGMENT_BYTES: usize = 32;

/// Bytes read from a file for one chunk of a [`MediaSource`] stream.
const SOURCE_CHUNK_BYTES: u64 = 2048;

/// Names drawn before storing gives up on finding a free one.
const NAME_ATTEMPTS: usize = 8;

/// What a channel knows about an attachment before its bytes arrive.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AttachmentMeta {
    /// How the channel presents the file.
    pub kind: MediaKind,
    /// MIME type the channel reported; `None` when it reported none.
    pub mime_type: Option<String>,
    /// Name the sender gave the file.
    pub filename: Option<String>,
}

impl AttachmentMeta {
    /// An attachment of `kind` with no reported type or name.
    #[must_use]
    pub const fn new(kind: MediaKind) -> Self {
        Self {
            kind,
            mime_type: None,
            filename: None,
        }
    }
}

/// Failure storing or opening an attachment.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum StoreError {
    /// The content exceeds the `attachment-max-bytes` setting.
    #[error("attachment exceeds {limit} bytes")]
    TooLarge {
        /// The setting's value.
        limit: u64,
    },
    /// The filesystem failed, or the path to open does not name a file.
    #[error("attachment filesystem failed: {0}")]
    Filesystem(FsError),
    /// The content's stream failed before it ended.
    #[error(transparent)]
    Stream(StreamError),
}

impl From<FsError> for StoreError {
    fn from(error: FsError) -> Self {
        Self::Filesystem(error)
    }
}

impl From<StreamError> for StoreError {
    fn from(error: StreamError) -> Self {
        Self::Stream(error)
    }
}

impl StoreError {
    /// The stable business error a Gateway operation answers with.
    pub(crate) const fn operation_error(&self) -> GatewayOperationError {
        match self {
            Self::TooLarge { .. } => GatewayOperationError::TooLarge,
            Self::Filesystem(
                FsError::NotFound
                | FsError::NotMounted
                | FsError::IsDirectory
                | FsError::NotDirectory
                | FsError::InvalidPath,
            ) => GatewayOperationError::NotFound,
            Self::Filesystem(_) | Self::Stream(_) => GatewayOperationError::Delivery,
        }
    }
}

/// The Gateway's attachment files: inbound storage and sending by path.
pub(crate) struct Attachments {
    filesystem: ScopedVfs,
    entropy: SharedEntropy,
    /// Random per boot, so names never repeat a previous boot's; drawn by
    /// the first attachment.
    prefix: OnceCell<String>,
    next: Cell<u32>,
    max_bytes: u64,
}

impl Attachments {
    pub(crate) const fn new(filesystem: ScopedVfs, entropy: SharedEntropy, max_bytes: u64) -> Self {
        Self {
            filesystem,
            entropy,
            prefix: OnceCell::new(),
            next: Cell::new(0),
            max_bytes,
        }
    }

    fn prefix(&self) -> &str {
        self.prefix.get_or_init(|| {
            let mut bytes = [0_u8; 4];
            if self.entropy.fill(&mut bytes).is_err() {
                log::warn!("no entropy for attachment names; relying on free-name checks");
            }
            let mut prefix = String::with_capacity(8);
            for byte in bytes {
                let _infallible = write!(prefix, "{byte:02x}");
            }
            prefix
        })
    }

    pub(crate) const fn max_bytes(&self) -> u64 {
        self.max_bytes
    }

    /// Writes `body` as a new attachment of `channel`.
    pub(crate) async fn store(
        &self,
        channel: &str,
        meta: AttachmentMeta,
        mut body: BinaryStream,
    ) -> Result<Attachment, StoreError> {
        let mut pending = self.begin(channel, meta).await?;
        while let Some(chunk) = body.next().await {
            let written = match chunk {
                Ok(bytes) => pending.write(&bytes).await,
                Err(error) => Err(StoreError::Stream(error)),
            };
            if let Err(error) = written {
                pending.abort().await;
                return Err(error);
            }
        }
        pending.finish().await
    }

    /// Opens a hidden partial file for a new attachment of `channel`.
    pub(crate) async fn begin(
        &self,
        channel: &str,
        meta: AttachmentMeta,
    ) -> Result<Pending<'_>, StoreError> {
        let directory = format!("{INBOUND_ROOT}/{}", channel_segment(channel));
        let mime_type = meta
            .mime_type
            .as_deref()
            .map(str::trim)
            .filter(|mime| !mime.is_empty())
            .map_or_else(|| String::from(UNKNOWN_MIME), String::from);
        let extension = mime_extension(&mime_type);
        let name = self.free_name(&directory, extension).await?;
        let path = format!("{directory}/{name}");
        let partial = format!("{directory}/.{name}.part");
        let mut options = OpenOptions::new();
        options.write(true).create(true).truncate(true);
        let file = self.filesystem.open_with(&partial, &options).await?;
        Ok(Pending {
            attachments: self,
            file: Some(file),
            partial,
            attachment: Attachment {
                kind: meta.kind,
                path,
                mime_type,
                filename: meta.filename,
                size: 0,
            },
        })
    }

    /// A name in `directory` that no file holds yet.
    async fn free_name(&self, directory: &str, extension: &str) -> Result<String, StoreError> {
        for _attempt in 0..NAME_ATTEMPTS {
            let counter = self.next.get();
            self.next.set(counter.wrapping_add(1));
            let name = format!("{}-{counter}.{extension}", self.prefix());
            match self
                .filesystem
                .metadata(&format!("{directory}/{name}"))
                .await
            {
                Err(FsError::NotFound) => return Ok(name),
                Ok(_taken) => {}
                Err(error) => return Err(error.into()),
            }
        }
        Err(StoreError::Filesystem(FsError::AlreadyExists))
    }

    /// Opens the file at `path` for sending.
    pub(crate) async fn open(&self, path: &str) -> Result<Rc<dyn MediaSource>, StoreError> {
        let metadata = self.filesystem.metadata(path).await?;
        if !metadata.is_file() {
            return Err(StoreError::Filesystem(FsError::IsDirectory));
        }
        if metadata.len() > self.max_bytes {
            return Err(StoreError::TooLarge {
                limit: self.max_bytes,
            });
        }
        Ok(Rc::new(FileSource {
            filesystem: self.filesystem.clone(),
            path: String::from(path),
            len: metadata.len(),
        }))
    }
}

impl MediaOpener for Attachments {
    async fn open_media(&self, path: &str) -> Result<Rc<dyn MediaSource>, GatewayOperationError> {
        self.open(path)
            .await
            .map_err(|error| error.operation_error())
    }
}

/// One attachment being written to its partial file.
pub(crate) struct Pending<'a> {
    attachments: &'a Attachments,
    /// `None` once closed.
    file: Option<File>,
    partial: String,
    attachment: Attachment,
}

impl Pending<'_> {
    /// Appends `bytes`, failing once the attachment would exceed the limit.
    pub(crate) async fn write(&mut self, bytes: &[u8]) -> Result<(), StoreError> {
        let limit = self.attachments.max_bytes;
        let size = u64::try_from(bytes.len())
            .ok()
            .and_then(|length| self.attachment.size.checked_add(length))
            .filter(|size| *size <= limit)
            .ok_or(StoreError::TooLarge { limit })?;
        let file = self
            .file
            .as_mut()
            .ok_or(StoreError::Filesystem(FsError::Io))?;
        file.write_all(bytes).await?;
        self.attachment.size = size;
        Ok(())
    }

    /// Closes the partial file and renames it into place.
    pub(crate) async fn finish(mut self) -> Result<Attachment, StoreError> {
        let closed = match self.file.take() {
            Some(mut file) => file.flush().await,
            None => Err(FsError::Io),
        };
        let renamed = match closed {
            Ok(()) => {
                self.attachments
                    .filesystem
                    .rename(&self.partial, &self.attachment.path)
                    .await
            }
            Err(error) => Err(error),
        };
        match renamed {
            Ok(()) => Ok(self.attachment),
            Err(error) => {
                self.abort().await;
                Err(error.into())
            }
        }
    }

    /// Closes and removes the partial file.
    pub(crate) async fn abort(mut self) {
        drop(self.file.take());
        if let Err(error) = self.attachments.filesystem.remove_file(&self.partial).await {
            log::warn!(
                "could not remove partial attachment {}: {error}",
                self.partial
            );
        }
    }
}

/// [`DownloadSink`] that stores a successful response as an attachment.
pub(crate) struct InboundSink<'a> {
    pub(crate) attachments: &'a Attachments,
    pub(crate) channel: &'a str,
    /// Taken by the head.
    pub(crate) meta: Option<AttachmentMeta>,
    pub(crate) pending: Option<Pending<'a>>,
}

/// Why an [`InboundSink`] stopped a download.
pub(crate) enum SinkError {
    Status(u16),
    Store(StoreError),
}

impl DownloadSink for InboundSink<'_> {
    type Error = SinkError;

    async fn head(&mut self, head: &DownloadHead) -> Result<(), SinkError> {
        if !(200..300).contains(&head.status) {
            return Err(SinkError::Status(head.status));
        }
        let mut meta = self
            .meta
            .take()
            .ok_or(SinkError::Store(StoreError::Filesystem(FsError::Io)))?;
        if meta.mime_type.is_none() {
            meta.mime_type = head
                .content_type
                .as_deref()
                .and_then(|value| value.split(';').next())
                .map(str::trim)
                .filter(|mime| !mime.is_empty())
                .map(String::from);
        }
        let pending = self
            .attachments
            .begin(self.channel, meta)
            .await
            .map_err(SinkError::Store)?;
        self.pending = Some(pending);
        Ok(())
    }

    async fn write(&mut self, bytes: &[u8]) -> Result<(), SinkError> {
        let pending = self
            .pending
            .as_mut()
            .ok_or(SinkError::Store(StoreError::Filesystem(FsError::Io)))?;
        pending.write(bytes).await.map_err(SinkError::Store)
    }
}

/// A file in the Gateway's view, read again from its start on every open.
struct FileSource {
    filesystem: ScopedVfs,
    path: String,
    len: u64,
}

impl MediaSource for FileSource {
    fn len(&self) -> u64 {
        self.len
    }

    fn open(&self) -> MediaOpenFuture<'_> {
        Box::pin(async move {
            let file = self.filesystem.open(&self.path).await.map_err(|error| {
                StreamError::failed(format!("cannot open {}: {error}", self.path))
            })?;
            Ok(file_stream(file, self.len))
        })
    }
}

/// Exactly `len` bytes of `file`, failing if it ends before them.
fn file_stream(file: File, len: u64) -> BinaryStream {
    Box::pin(stream::unfold(Some((file, len)), |state| async move {
        let (mut file, remaining) = state?;
        if remaining == 0 {
            return None;
        }
        let want = usize::try_from(remaining.min(SOURCE_CHUNK_BYTES)).unwrap_or(0);
        let mut bytes = vec![0_u8; want];
        match file.read(&mut bytes).await {
            Ok(0) => Some((Err(StreamError::failed("file ended early")), None)),
            Ok(read) => {
                bytes.truncate(read);
                let rest = u64::try_from(read)
                    .ok()
                    .and_then(|read| remaining.checked_sub(read))
                    .unwrap_or(0);
                Some((Ok(BinaryChunk::from(bytes)), Some((file, rest))))
            }
            Err(error) => Some((
                Err(StreamError::failed(format!("file read failed: {error}"))),
                None,
            )),
        }
    }))
}

/// `channel` as one safe directory name: ASCII letters, digits, `-`, and
/// `_`, everything else as `_`, at most [`CHANNEL_SEGMENT_BYTES`] long.
fn channel_segment(channel: &str) -> String {
    let mut segment: String = channel
        .chars()
        .take(CHANNEL_SEGMENT_BYTES)
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '-' | '_') {
                character
            } else {
                '_'
            }
        })
        .collect();
    if segment.is_empty() {
        segment.push('_');
    }
    segment
}

#[cfg(test)]
mod tests {
    use super::channel_segment;

    #[test]
    fn a_channel_name_becomes_one_safe_directory_name() {
        assert_eq!(channel_segment("telegram"), "telegram");
        assert_eq!(channel_segment("we-chat_2"), "we-chat_2");
        assert_eq!(channel_segment("../etc"), "___etc");
        assert_eq!(channel_segment("a/b c"), "a_b_c");
        assert_eq!(channel_segment(""), "_");
        assert_eq!(channel_segment(&"x".repeat(40)).len(), 32);
        assert_eq!(channel_segment("微信"), "__");
    }
}
