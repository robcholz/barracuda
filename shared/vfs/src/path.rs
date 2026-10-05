use alloc::borrow::Cow;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use crate::FsError;

pub(crate) fn normalize(path: &str) -> Result<String, FsError> {
    if !path.starts_with('/') || path.as_bytes().contains(&0) {
        return Err(FsError::InvalidPath);
    }

    let mut components = Vec::new();
    for component in path.split('/') {
        match component {
            "" | "." => {}
            ".." => {
                if components.pop().is_none() {
                    return Err(FsError::InvalidPath);
                }
            }
            value => components.push(value),
        }
    }

    if components.is_empty() {
        return Ok("/".to_string());
    }

    let mut normalized = String::new();
    for component in components {
        normalized.push('/');
        normalized.push_str(component);
    }
    Ok(normalized)
}

/// Normalizes a path that lives as long as the caller keeps it.
///
/// Already normalized text is kept as given, so static paths stay borrowed;
/// owned results are shrunk to their length.
pub(crate) fn normalize_retained(path: Cow<'static, str>) -> Result<Cow<'static, str>, FsError> {
    if !is_normalized(&path) {
        return normalize(&path).map(|mut normalized| {
            normalized.shrink_to_fit();
            Cow::Owned(normalized)
        });
    }
    Ok(match path {
        Cow::Owned(mut owned) => {
            owned.shrink_to_fit();
            Cow::Owned(owned)
        }
        borrowed @ Cow::Borrowed(_) => borrowed,
    })
}

/// Whether `normalize` would return `path` unchanged.
fn is_normalized(path: &str) -> bool {
    path == "/"
        || (path.starts_with('/')
            && !path.as_bytes().contains(&0)
            && path
                .split('/')
                .skip(1)
                .all(|component| !matches!(component, "" | "." | "..")))
}

pub(crate) fn matches_mount(path: &str, mount_point: &str) -> bool {
    mount_point == "/"
        || path == mount_point
        || path
            .strip_prefix(mount_point)
            .is_some_and(|suffix| suffix.starts_with('/'))
}

pub(crate) fn backend_path(path: &str, mount_point: &str, source_root: &str) -> String {
    let suffix = if mount_point == "/" {
        path
    } else if path == mount_point {
        "/"
    } else {
        path.strip_prefix(mount_point).unwrap_or("/")
    };

    if source_root == "/" {
        suffix.to_string()
    } else if suffix == "/" {
        source_root.to_string()
    } else {
        let mut joined = source_root.to_string();
        joined.push_str(suffix);
        joined
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retained_paths_borrow_only_already_normalized_static_text() {
        for path in ["/", "/a", "/a/b.c", "/a/..b"] {
            assert!(matches!(
                normalize_retained(Cow::Borrowed(path)),
                Ok(Cow::Borrowed(kept)) if kept == path
            ));
        }
        for (path, expected) in [("//a", "/a"), ("/a/", "/a"), ("/a/./b/../c", "/a/c")] {
            assert!(matches!(
                normalize_retained(Cow::Borrowed(path)),
                Ok(Cow::Owned(normalized)) if normalized == expected
            ));
        }
        for path in ["a", "/..", "/a\0"] {
            assert!(normalize_retained(Cow::Borrowed(path)).is_err());
        }
    }
}
