//! Inline image bytes are base64-encoded while the request body is written,
//! never as a separate copy. Filesystem access belongs to the
//! platform/application layer, which can read through their filesystem adapter
//! and construct [`MediaAsset::inline_bytes`].

use super::errors::Error;
use super::types::MediaAsset;

#[derive(Clone, Copy, Debug)]
pub(crate) enum Prepared<'a> {
    Inline { mime_type: &'a str, bytes: &'a [u8] },
    RemoteUrl(&'a str),
}

fn check_inline_bytes(bytes: &[u8], image_max_bytes: usize) -> Result<(), Error> {
    if bytes.is_empty() {
        return Err(Error::MediaFileEmpty);
    }
    if bytes.len() > image_max_bytes {
        return Err(Error::MediaTooLarge);
    }
    Ok(())
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
            check_inline_bytes(bytes, image_max_bytes)?;
            Ok(Prepared::Inline { mime_type, bytes })
        }
    }
}
