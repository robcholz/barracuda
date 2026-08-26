//! Inline image bytes are base64-encoded with the `base64` crate. Filesystem
//! access belongs to the platform/application layer, which can read through
//! their filesystem adapter and construct [`MediaAsset::inline_bytes`].

use alloc::borrow::Cow;
use alloc::format;
use alloc::string::String;
use base64::engine::general_purpose::STANDARD;
use base64::Engine;

use super::errors::Error;
use super::types::MediaAsset;

#[derive(Debug)]
pub(crate) enum Prepared<'a> {
    Inline { mime_type: &'a str, base64: String },
    RemoteUrl(&'a str),
}

impl Prepared<'_> {
    pub(crate) fn openai_url(&self) -> Cow<'_, str> {
        match self {
            Self::Inline { mime_type, base64 } => {
                Cow::Owned(format!("data:{mime_type};base64,{base64}"))
            }
            Self::RemoteUrl(url) => Cow::Borrowed(url),
        }
    }
}

fn prepare_inline_bytes_asset(bytes: &[u8], image_max_bytes: usize) -> Result<String, Error> {
    if bytes.is_empty() {
        return Err(Error::MediaFileEmpty);
    }
    if bytes.len() > image_max_bytes {
        return Err(Error::MediaTooLarge);
    }

    Ok(STANDARD.encode(bytes))
}

pub(crate) fn prepare_asset<'a>(
    asset: &'a MediaAsset,
    image_max_bytes: usize,
) -> Result<Prepared<'a>, Error> {
    match asset {
        MediaAsset::RemoteUrl { url } => {
            if url.is_empty() {
                return Err(Error::MediaUrlEmpty);
            }
            Ok(Prepared::RemoteUrl(url))
        }
        MediaAsset::InlineBytes { bytes, mime_type } => {
            prepare_inline_bytes_asset(bytes, image_max_bytes)
                .map(|base64| Prepared::Inline { mime_type, base64 })
        }
    }
}
