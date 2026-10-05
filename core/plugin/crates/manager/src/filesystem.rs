//! Plugin-facing restricted VFS namespaces.

use alloc::borrow::Cow;
use alloc::format;

use barracuda_vfs::{FsError, ScopedVfs, Vfs};

use crate::PluginId;

pub(crate) fn scoped_filesystem(vfs: &Vfs, plugin_id: &PluginId) -> Result<ScopedVfs, FsError> {
    vfs.scoped_mounts([
        (
            "/resources",
            Cow::Owned(format!("/resources/plugins/{plugin_id}")),
        ),
        ("/data", Cow::Owned(format!("/data/plugins/{plugin_id}"))),
        ("/cache", Cow::Owned(format!("/cache/plugins/{plugin_id}"))),
        ("/media", Cow::Owned(format!("/media/plugins/{plugin_id}"))),
        (
            "/workspace/resources",
            Cow::Borrowed("/resources/workspace"),
        ),
        ("/workspace/cache", Cow::Borrowed("/cache/workspace")),
        ("/workspace/media", Cow::Borrowed("/media/workspace")),
        ("/workspace/removable", Cow::Borrowed("/removable")),
    ])
}
