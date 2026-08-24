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
