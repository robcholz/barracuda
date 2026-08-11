//! Media preparation, port of `claw_media_pipeline.c`.
//!
//! Inline image bytes are base64-encoded with the `base64` crate. Filesystem
//! access belongs to the platform/application layer, which can read through
//! their filesystem adapter and construct [`MediaAsset::inline_bytes`].

use alloc::format;
use alloc::string::String;
use base64::engine::general_purpose::STANDARD;
use base64::Engine;

use super::errors::InferMediaError;
use super::types::MediaAsset;

/// How a prepared media payload is encoded (`claw_media_prepared_kind_t`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PreparedKind {
    DataUrl,
    RemoteUrl,
}

/// Output of the media-prep pipeline (`claw_media_prepared_t`).
#[derive(Clone, Debug)]
pub(crate) struct Prepared {
    kind: PreparedKind,
    /// Data URL (for [`PreparedKind::DataUrl`]) or the remote URL.
    payload: String,
}

impl Prepared {
    pub(crate) fn is_data_url(&self) -> bool {
        self.kind == PreparedKind::DataUrl
    }

    pub(crate) fn payload(&self) -> &str {
        &self.payload
    }
}

fn prepare_inline_bytes_asset(
    bytes: &[u8],
    mime: &str,
    image_max_bytes: usize,
) -> Result<Prepared, InferMediaError> {
    if bytes.is_empty() {
        return Err(InferMediaError::MediaFileEmpty);
    }
    if bytes.len() > image_max_bytes {
        return Err(InferMediaError::MediaTooLarge);
    }

    let encoded = STANDARD.encode(bytes);
    let payload = format!("data:{mime};base64,{encoded}");

    Ok(Prepared {
        kind: PreparedKind::DataUrl,
        payload,
    })
}

/// `claw_media_prepare_asset`
pub(crate) fn prepare_asset(
    asset: &MediaAsset,
    image_remote_url_only: bool,
    image_max_bytes: usize,
) -> Result<Prepared, InferMediaError> {
    match asset {
        MediaAsset::RemoteUrl { url } => {
            if url.is_empty() {
                return Err(InferMediaError::MediaUrlEmpty);
            }
            Ok(Prepared {
                kind: PreparedKind::RemoteUrl,
                payload: url.clone(),
            })
        }
        MediaAsset::InlineBytes { bytes, mime_type } => {
            if image_remote_url_only {
                return Err(InferMediaError::RemoteOnlyProfile);
            }
            prepare_inline_bytes_asset(bytes, mime_type, image_max_bytes)
        }
    }
}
