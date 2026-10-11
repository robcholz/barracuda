//! Files the Web channel shows: uploads waiting for the message that sends
//! them, and files sent to pages, each under an opaque id.
//!
//! Only references are kept: an upload is the Gateway's [`Attachment`], a sent
//! file a re-openable [`MediaSource`]. Both shelves are bounded and evict their
//! oldest entry, so a page may find an id gone and must say so.

use alloc::{
    boxed::Box,
    collections::VecDeque,
    format,
    rc::Rc,
    string::{String, ToString},
    vec::Vec,
};
use core::{
    cell::{Cell, OnceCell, RefCell},
    fmt::Write as _,
    future::Future,
    pin::Pin,
};

use barracuda_imessage_gateway_plugin::{
    Attachment, BinaryStream, ChannelError, MediaKind, MediaSource,
};
use barracuda_platform::Entropy as _;
use barracuda_plugin::api::SharedEntropy;

/// Path prefix of a sent file's URL; the id follows.
pub const MEDIA_PATH: &str = "/api/web/media/";
/// Sent files kept for download at once; the oldest is evicted first.
pub const MAX_KEPT: usize = 32;
/// Uploads kept until a message frame sends them; the oldest is evicted first.
pub const MAX_UPLOADS: usize = 16;
/// Attachments one message frame may name.
pub const MAX_ATTACHMENTS: usize = 4;
/// MIME type of a file whose type nobody named.
pub const UNKNOWN_MIME: &str = "application/octet-stream";

/// Future answered by a [`MediaSpool`].
pub type SpoolFuture<'a> =
    Pin<Box<dyn Future<Output = Result<Rc<dyn MediaSource>, ChannelError>> + 'a>>;

/// Storage for media handed to the channel as bytes or a stream, so pages can
/// fetch it later without the channel keeping the bytes in memory.
pub trait MediaSpool: 'static {
    /// Writes `body` to storage and opens it again as a source of known length.
    fn spool(
        &self,
        kind: MediaKind,
        mime_type: String,
        filename: Option<String>,
        body: BinaryStream,
    ) -> SpoolFuture<'_>;
}

/// One file sent to pages.
#[derive(Clone)]
pub struct KeptMedia {
    /// The file, opened again for each download.
    pub source: Rc<dyn MediaSource>,
    /// MIME type the sender named.
    pub mime_type: String,
    /// Name the sender gave the file.
    pub filename: Option<String>,
}

/// The channel's bounded shelves of uploads and sent files.
pub struct WebMedia {
    entropy: SharedEntropy,
    /// Random per boot, so a page holding an id from before a restart finds
    /// nothing rather than another file; drawn by the first id.
    prefix: OnceCell<String>,
    next: Cell<u32>,
    kept: RefCell<VecDeque<(String, KeptMedia)>>,
    uploads: RefCell<VecDeque<(String, Attachment)>>,
}

impl WebMedia {
    /// Empty shelves minting ids from `entropy`.
    #[must_use]
    pub fn new(entropy: SharedEntropy) -> Self {
        Self {
            entropy,
            prefix: OnceCell::new(),
            next: Cell::new(0),
            kept: RefCell::new(VecDeque::new()),
            uploads: RefCell::new(VecDeque::new()),
        }
    }

    /// A new opaque id: the boot's prefix and a counter, such as `1a2b3c4d-7`.
    fn mint(&self) -> String {
        let prefix = self.prefix.get_or_init(|| {
            let mut bytes = [0_u8; 4];
            if self.entropy.fill(&mut bytes).is_err() {
                log::warn!("IMessage Web has no entropy for media ids");
            }
            let mut prefix = String::with_capacity(8);
            for byte in bytes {
                let _infallible = write!(prefix, "{byte:02x}");
            }
            prefix
        });
        let counter = self.next.get();
        self.next.set(counter.wrapping_add(1));
        format!("{prefix}-{counter}")
    }

    /// Keeps `media` for download, evicting the oldest past [`MAX_KEPT`].
    pub fn keep(&self, media: KeptMedia) -> String {
        let id = self.mint();
        push_bounded(&self.kept, id.clone(), media, MAX_KEPT);
        id
    }

    /// The sent file kept under `id`.
    #[must_use]
    pub fn kept(&self, id: &str) -> Option<KeptMedia> {
        self.kept
            .borrow()
            .iter()
            .find(|(kept, _media)| kept == id)
            .map(|(_id, media)| media.clone())
    }

    /// Holds an upload until a message sends it, evicting the oldest past
    /// [`MAX_UPLOADS`].
    pub fn hold(&self, attachment: Attachment) -> String {
        let id = self.mint();
        push_bounded(&self.uploads, id.clone(), attachment, MAX_UPLOADS);
        id
    }

    /// Takes the uploads `ids` name, in their order, or none of them when
    /// any id is unknown (never uploaded, already sent, or evicted) or named
    /// twice.
    pub fn take(&self, ids: &[String]) -> Option<Vec<Attachment>> {
        let mut uploads = self.uploads.borrow_mut();
        let mut positions = Vec::with_capacity(ids.len());
        for id in ids {
            let position = uploads.iter().position(|(held, _upload)| held == id)?;
            if positions.contains(&position) {
                return None;
            }
            positions.push(position);
        }
        let taken = positions
            .iter()
            .filter_map(|&position| uploads.get(position).map(|(_id, upload)| upload.clone()))
            .collect();
        uploads.retain(|(held, _upload)| !ids.contains(held));
        Some(taken)
    }
}

fn push_bounded<T>(shelf: &RefCell<VecDeque<(String, T)>>, id: String, value: T, bound: usize) {
    let mut shelf = shelf.borrow_mut();
    while shelf.len() >= bound {
        if let Some((evicted, _value)) = shelf.pop_front() {
            log::debug!("IMessage Web evicted media `{evicted}`");
        }
    }
    shelf.push_back((id, value));
}

/// The MIME type a sender named, else the one `filename`'s extension names,
/// else [`UNKNOWN_MIME`].
pub(crate) fn mime_or_default(mime_type: Option<&str>, filename: Option<&str>) -> String {
    mime_type
        .map(str::trim)
        .filter(|mime| !mime.is_empty())
        .map(String::from)
        .or_else(|| {
            filename
                .and_then(barracuda_imessage_gateway_plugin::extension_mime)
                .map(String::from)
        })
        .unwrap_or_else(|| UNKNOWN_MIME.to_string())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::indexing_slicing, clippy::unwrap_used)]

    use alloc::{format, string::String, vec, vec::Vec};

    use barracuda_imessage_gateway_plugin::{Attachment, MediaKind};
    use barracuda_plugin::api::SharedEntropy;

    use super::{mime_or_default, WebMedia, MAX_UPLOADS};

    fn upload(name: &str) -> Attachment {
        Attachment {
            kind: MediaKind::Image,
            path: format!("/workspace/cache/gateway/web/{name}"),
            mime_type: "image/png".into(),
            filename: Some(name.into()),
            size: 3,
        }
    }

    #[test]
    fn uploads_are_taken_once_all_or_nothing_and_the_oldest_is_evicted() {
        let media = WebMedia::new(SharedEntropy::unavailable());
        let first = media.hold(upload("a.png"));
        let second = media.hold(upload("b.png"));
        assert_ne!(first, second);
        assert_eq!(media.take(&[first.clone(), "nope".into()]), None);
        assert_eq!(media.take(&[first.clone(), first.clone()]), None);
        let taken = media.take(&[second.clone(), first.clone()]).unwrap();
        assert_eq!(taken, vec![upload("b.png"), upload("a.png")]);
        assert_eq!(media.take(&[first]), None);

        let ids: Vec<String> = (0..=MAX_UPLOADS)
            .map(|index| media.hold(upload(&format!("{index}.png"))))
            .collect();
        assert_eq!(media.take(&ids[..1]), None);
        assert!(media.take(&ids[1..]).is_some());
    }

    #[test]
    fn a_missing_mime_type_comes_from_the_name_else_bytes() {
        assert_eq!(mime_or_default(Some("audio/ogg"), None), "audio/ogg");
        assert_eq!(mime_or_default(Some(" "), Some("a.png")), "image/png");
        assert_eq!(
            mime_or_default(None, Some("a.zip")),
            "application/octet-stream"
        );
    }
}
