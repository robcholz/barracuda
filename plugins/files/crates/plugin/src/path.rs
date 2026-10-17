//! The paths the API accepts: real scoped paths beneath the Workspace and the
//! Plugin trees, one validated name at a time.

use alloc::string::String;
use alloc::vec::Vec;

const MAX_PATH: usize = 1024;
const MAX_NAME: usize = 255;
/// The only trees the browser reaches; the Plugin's own namespace stays out.
const ROOTS: [&str; 2] = ["workspace", "plugins"];

/// Decodes a percent-encoded URL tail, without its leading slash, into an
/// absolute scoped path, or `None` when the result is not an accepted path.
pub(crate) fn decode(encoded: &str) -> Option<String> {
    let mut decoded = Vec::with_capacity(encoded.len() + 1);
    decoded.push(b'/');
    let mut bytes = encoded.bytes();
    while let Some(byte) = bytes.next() {
        if byte == b'%' {
            let high = hex(bytes.next()?)?;
            let low = hex(bytes.next()?)?;
            decoded.push((high << 4) | low);
        } else {
            decoded.push(byte);
        }
    }
    let path = String::from_utf8(decoded).ok()?;
    accepted(&path).then_some(path)
}

/// Whether `path` is absolute, beneath an allowed root, and made of valid names.
pub(crate) fn accepted(path: &str) -> bool {
    path.len() <= MAX_PATH
        && path.strip_prefix('/').is_some_and(|relative| {
            relative
                .split('/')
                .next()
                .is_some_and(|root| ROOTS.contains(&root))
                && relative.split('/').all(valid_name)
        })
}

/// One path segment: non-empty, not `.` or `..`, without separators or
/// control characters.
pub(crate) fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= MAX_NAME
        && name != "."
        && name != ".."
        && !name
            .chars()
            .any(|character| character == '/' || character == '\\' || character.is_control())
}

/// Splits an accepted path into its parent and final name.
pub(crate) fn split(path: &str) -> (&str, &str) {
    path.rsplit_once('/').unwrap_or(("", path))
}

const fn hex(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::{accepted, decode, split};

    #[test]
    fn decodes_names_and_rejects_escapes() {
        assert_eq!(
            decode("workspace/media/a%20b%E2%9C%93.txt").as_deref(),
            Some("/workspace/media/a b✓.txt")
        );
        assert_eq!(decode("plugins").as_deref(), Some("/plugins"));
        for rejected in [
            "",
            "data/state",
            "workspace/../data",
            "workspace/%2E%2E/data",
            "workspace/a%5Cb",
            "workspace/a%00",
            "workspace//a",
            "workspace/a/",
            "workspace/%zz",
            "workspace/%E2%9C",
        ] {
            assert_eq!(decode(rejected), None, "{rejected}");
        }
        assert!(!accepted("workspace/media"));
        assert_eq!(
            split("/workspace/media/a.txt"),
            ("/workspace/media", "a.txt")
        );
    }
}
