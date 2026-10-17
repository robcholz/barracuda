//! Inbound files: an owner's image, voice, file, and video items fetched
//! from the iLink CDN, decrypted block by block, and stored by the Gateway.
//!
//! The download (a [`DownloadSink`] that decrypts each read) and the
//! Gateway's storing (a [`BinaryStream`] it pulls) run side by side, joined
//! by a two-piece channel, so a file of any size passes through a few
//! kilobytes of RAM.

use alloc::{boxed::Box, format, rc::Rc, string::String, vec::Vec};

use barracuda_imessage_gateway_plugin::{
    Attachment, AttachmentMeta, BinaryChunk, DownloadError, DownloadHead, DownloadLimits,
    DownloadRequest, DownloadSink, Error as HttpError, IMessageGateway, InboundDownloadError,
    MediaKind, StoreError, StreamError, DOWNLOAD_IDLE_TIMEOUT,
};
use barracuda_tls::{Aes128EcbDecryptor, AES_128_KEY_LEN, AES_BLOCK_LEN};
use embassy_futures::select::{select, Either};
use embassy_sync::{blocking_mutex::raw::NoopRawMutex, channel::Channel};
use futures_lite::stream::{self, StreamExt as _};
use wechat::{media::sniff_image, InboundMedia, Wechat};

use crate::CHANNEL;

/// Why a file was not stored.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Missed {
    /// Over the Gateway's `attachment-max-bytes`.
    TooLarge,
    /// No reference or key, or the download, decryption, or storing failed.
    Failed,
}

/// Appends the line that tells the reader a file did not arrive.
pub(crate) fn append_note(text: &mut String, kind: MediaKind, missed: Missed) {
    let kind = match kind {
        MediaKind::File => "file",
        MediaKind::Image => "image",
        MediaKind::Audio => "audio",
        MediaKind::Video => "video",
    };
    if !text.is_empty() {
        text.push('\n');
    }
    match missed {
        Missed::TooLarge => text.push_str(&format!("[{kind} too large, not received]")),
        Missed::Failed => text.push_str(&format!("[{kind} not received]")),
    }
}

/// Downloads `media` from the CDN into the Gateway's attachment storage,
/// decrypting it unless it is an image sent without a key.
pub(crate) async fn fetch(
    gateway: &IMessageGateway,
    provider: &Wechat,
    media: InboundMedia,
) -> Result<Attachment, Missed> {
    if media
        .size
        .is_some_and(|size| size > gateway.attachment_max_bytes())
    {
        return Err(Missed::TooLarge);
    }
    let url = media
        .media
        .as_ref()
        .filter(|_reference| media.fetchable())
        .and_then(|reference| provider.download_url(reference));
    let Some(url) = url else {
        log::warn!("a WeChat file came without a usable reference or key");
        return Err(Missed::Failed);
    };
    let meta = AttachmentMeta {
        kind: media.kind,
        mime_type: media.mime_type,
        filename: media.filename,
    };
    // The URL is a credential for the file: never log it.
    match media.key {
        Some(key) => decrypted(gateway, &url, &key, meta).await,
        None => match gateway
            .download_inbound(CHANNEL, DownloadRequest::get(&url), meta)
            .await
        {
            Ok(attachment) => Ok(attachment),
            Err(InboundDownloadError::Store(StoreError::TooLarge { .. })) => Err(Missed::TooLarge),
            Err(error) => {
                log::warn!("failed to download a WeChat file: {error}");
                Err(Missed::Failed)
            }
        },
    }
}

/// One piece handed from the download to the storing.
enum Piece {
    Bytes(Vec<u8>),
    End,
    Failed,
}

type Pipe = Channel<NoopRawMutex, Piece, 2>;

/// Decrypts each read of the body into the pipe.
struct Decrypting {
    decryptor: Option<Aes128EcbDecryptor>,
    pipe: Rc<Pipe>,
}

/// Why [`Decrypting`] stopped the download.
#[derive(Debug)]
enum SinkError {
    Status(u16),
    Cipher,
}

impl DownloadSink for Decrypting {
    type Error = SinkError;

    async fn head(&mut self, head: &DownloadHead) -> Result<(), SinkError> {
        if (200..300).contains(&head.status) {
            Ok(())
        } else {
            Err(SinkError::Status(head.status))
        }
    }

    async fn write(&mut self, bytes: &[u8]) -> Result<(), SinkError> {
        let decryptor = self.decryptor.as_mut().ok_or(SinkError::Cipher)?;
        let mut plaintext = Vec::with_capacity(bytes.len().saturating_add(AES_BLOCK_LEN));
        decryptor
            .update(bytes, &mut plaintext)
            .map_err(|_error| SinkError::Cipher)?;
        if !plaintext.is_empty() {
            self.pipe.send(Piece::Bytes(plaintext)).await;
        }
        Ok(())
    }
}

/// Downloads `url` through AES-128-ECB with `key` into the Gateway's
/// storage.
async fn decrypted(
    gateway: &IMessageGateway,
    url: &str,
    key: &[u8; AES_128_KEY_LEN],
    meta: AttachmentMeta,
) -> Result<Attachment, Missed> {
    let pipe = Rc::new(Pipe::new());
    let decryptor = Aes128EcbDecryptor::new(key).map_err(|_error| Missed::Failed)?;
    let mut sink = Decrypting {
        decryptor: Some(decryptor),
        pipe: Rc::clone(&pipe),
    };
    let max_bytes = gateway.attachment_max_bytes();
    // The ciphertext is at most one block longer than the file.
    let limits = DownloadLimits {
        max_bytes: max_bytes.saturating_add(AES_BLOCK_LEN as u64),
        idle_timeout: DOWNLOAD_IDLE_TIMEOUT,
    };
    let http = gateway.http();
    let downloading = async {
        let downloaded = http
            .download(DownloadRequest::get(url), limits, &mut sink)
            .await;
        let outcome = match downloaded {
            Ok(_head) => {
                let mut rest = Vec::new();
                let finished = sink
                    .decryptor
                    .take()
                    .map(|decryptor| decryptor.finish(&mut rest));
                match finished {
                    Some(Ok(())) => {
                        if !rest.is_empty() {
                            pipe.send(Piece::Bytes(rest)).await;
                        }
                        pipe.send(Piece::End).await;
                        return Ok(());
                    }
                    _ => {
                        log::warn!("a WeChat file did not decrypt");
                        Missed::Failed
                    }
                }
            }
            Err(DownloadError::Http(HttpError::BodyTooLarge)) => Missed::TooLarge,
            Err(DownloadError::Sink(SinkError::Status(status))) => {
                log::warn!("the WeChat CDN answered HTTP {status}");
                Missed::Failed
            }
            Err(DownloadError::Sink(SinkError::Cipher)) => {
                log::warn!("a WeChat file did not decrypt");
                Missed::Failed
            }
            Err(DownloadError::Http(error)) => {
                log::warn!("failed to download a WeChat file: {error}");
                Missed::Failed
            }
        };
        pipe.send(Piece::Failed).await;
        Err(outcome)
    };
    let mut storing = core::pin::pin!(store(gateway, meta, Rc::clone(&pipe)));
    match select(storing.as_mut(), downloading).await {
        // Storing ends first only when it failed; the download stops.
        Either::First(stored) => stored,
        Either::Second(downloaded) => {
            let stored = storing.await;
            downloaded?;
            stored
        }
    }
}

/// Stores what the pipe carries; an image without a type is named by its
/// first bytes.
async fn store(
    gateway: &IMessageGateway,
    mut meta: AttachmentMeta,
    pipe: Rc<Pipe>,
) -> Result<Attachment, Missed> {
    let (first, rest) = match pipe.receive().await {
        Piece::Bytes(bytes) => (bytes, Some(pipe)),
        Piece::End => (Vec::new(), None),
        Piece::Failed => return Err(Missed::Failed),
    };
    if meta.mime_type.is_none() && meta.kind == MediaKind::Image {
        meta.mime_type = sniff_image(&first).map(String::from);
    }
    let rest = stream::unfold(rest, |pipe| async move {
        let pipe = pipe?;
        match pipe.receive().await {
            Piece::Bytes(bytes) => Some((Ok(BinaryChunk::from(bytes)), Some(pipe))),
            Piece::End => None,
            Piece::Failed => Some((Err(StreamError::failed("the download failed")), None)),
        }
    });
    let body = stream::once(Ok(BinaryChunk::from(first))).chain(rest);
    match gateway.store_inbound(CHANNEL, meta, Box::pin(body)).await {
        Ok(attachment) => Ok(attachment),
        Err(StoreError::TooLarge { .. }) => Err(Missed::TooLarge),
        Err(StoreError::Stream(_failed)) => Err(Missed::Failed),
        Err(error) => {
            log::warn!("failed to store a WeChat file: {error}");
            Err(Missed::Failed)
        }
    }
}
