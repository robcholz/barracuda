//! Plugin-facing restricted VFS namespaces.

use alloc::format;

use barracuda_vfs::{FsError, ScopedVfs, Vfs};

use crate::PluginId;

pub(crate) fn scoped_filesystem(vfs: &Vfs, plugin_id: &PluginId) -> Result<ScopedVfs, FsError> {
    vfs.scoped_mounts([
        ("/resources", format!("/resources/plugins/{plugin_id}")),
        ("/data", format!("/data/plugins/{plugin_id}")),
        ("/cache", format!("/cache/plugins/{plugin_id}")),
        ("/media", format!("/media/plugins/{plugin_id}")),
    ])
}
