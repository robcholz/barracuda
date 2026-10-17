//! MIME types and file extensions of media the Gateway names on disk.

/// Known MIME types and the extension a stored file of that type gets; the
/// first row of an extension is its MIME type.
const TYPES: &[(&str, &str)] = &[
    ("image/jpeg", "jpg"),
    ("image/jpg", "jpg"),
    ("image/pjpeg", "jpg"),
    ("image/png", "png"),
    ("image/gif", "gif"),
    ("image/webp", "webp"),
    ("image/heic", "heic"),
    ("video/mp4", "mp4"),
    ("video/quicktime", "mov"),
    ("audio/ogg", "ogg"),
    ("application/ogg", "ogg"),
    ("audio/opus", "opus"),
    ("audio/mpeg", "mp3"),
    ("audio/mp3", "mp3"),
    ("audio/mp4", "m4a"),
    ("audio/m4a", "m4a"),
    ("audio/x-m4a", "m4a"),
    ("audio/wav", "wav"),
    ("audio/x-wav", "wav"),
    ("audio/wave", "wav"),
    ("application/pdf", "pdf"),
    ("text/plain", "txt"),
];

/// Extensions that only ever name a file, mapped to their MIME type.
const EXTENSION_ALIASES: &[(&str, &str)] = &[("jpeg", "image/jpeg"), ("oga", "audio/ogg")];

/// Extension used for a MIME type outside the table.
pub const DEFAULT_EXTENSION: &str = "bin";

/// The file extension, without the dot, for a stored file of `mime_type`.
///
/// Parameters (`; charset=…`) and case are ignored. A type outside the
/// table gets [`DEFAULT_EXTENSION`].
#[must_use]
pub fn mime_extension(mime_type: &str) -> &'static str {
    let essence = mime_type.split(';').next().unwrap_or_default().trim();
    TYPES
        .iter()
        .find(|(mime, _extension)| mime.eq_ignore_ascii_case(essence))
        .map_or(DEFAULT_EXTENSION, |&(_mime, extension)| extension)
}

/// The MIME type of the file at `path`, from its name's extension, when the
/// table knows it.
#[must_use]
pub fn extension_mime(path: &str) -> Option<&'static str> {
    let name = path.rsplit('/').next().unwrap_or(path);
    let (_stem, extension) = name.rsplit_once('.')?;
    TYPES
        .iter()
        .map(|&(mime, extension)| (extension, mime))
        .chain(EXTENSION_ALIASES.iter().copied())
        .find(|(known, _mime)| known.eq_ignore_ascii_case(extension))
        .map(|(_extension, mime)| mime)
}

#[cfg(test)]
mod tests {
    use super::{extension_mime, mime_extension};

    #[test]
    fn a_mime_type_names_its_extension_ignoring_case_and_parameters() {
        assert_eq!(mime_extension("image/jpeg"), "jpg");
        assert_eq!(mime_extension("Audio/OGG; codecs=opus"), "ogg");
        assert_eq!(mime_extension("text/plain; charset=utf-8"), "txt");
        assert_eq!(mime_extension("video/quicktime"), "mov");
        assert_eq!(mime_extension("application/zip"), "bin");
        assert_eq!(mime_extension(""), "bin");
    }

    #[test]
    fn an_extension_names_its_mime_type() {
        assert_eq!(extension_mime("photo.JPG"), Some("image/jpeg"));
        assert_eq!(extension_mime("photo.jpeg"), Some("image/jpeg"));
        assert_eq!(
            extension_mime("/workspace/cache/voice.oga"),
            Some("audio/ogg")
        );
        assert_eq!(extension_mime("clip.m4a"), Some("audio/mp4"));
        assert_eq!(extension_mime("archive.zip"), None);
        assert_eq!(extension_mime("README"), None);
        assert_eq!(extension_mime("/data/v1.2/README"), None);
    }
}
