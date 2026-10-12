//! Plugin-facing restricted VFS namespaces.

use alloc::borrow::Cow;
use alloc::format;

use barracuda_vfs::{FsError, MountOptions, ScopedVfs, Vfs};

use crate::PluginId;

/// Every Plugin's private trees, as an inspecting Plugin sees them.
const PLUGIN_TREES: [(&str, &str); 4] = [
    ("/plugins/data", "/data/plugins"),
    ("/plugins/cache", "/cache/plugins"),
    ("/plugins/media", "/media/plugins"),
    ("/plugins/resources", "/resources/plugins"),
];

pub(crate) fn scoped_filesystem(
    vfs: &Vfs,
    plugin_id: &PluginId,
    inspect: bool,
) -> Result<ScopedVfs, FsError> {
    let private: [(Cow<'static, str>, Cow<'static, str>); 8] = [
        (
            Cow::Borrowed("/resources"),
            Cow::Owned(format!("/resources/plugins/{plugin_id}")),
        ),
        (
            Cow::Borrowed("/data"),
            Cow::Owned(format!("/data/plugins/{plugin_id}")),
        ),
        (
            Cow::Borrowed("/cache"),
            Cow::Owned(format!("/cache/plugins/{plugin_id}")),
        ),
        (
            Cow::Borrowed("/media"),
            Cow::Owned(format!("/media/plugins/{plugin_id}")),
        ),
        (
            Cow::Borrowed("/workspace/resources"),
            Cow::Borrowed("/resources/workspace"),
        ),
        (
            Cow::Borrowed("/workspace/cache"),
            Cow::Borrowed("/cache/workspace"),
        ),
        (
            Cow::Borrowed("/workspace/media"),
            Cow::Borrowed("/media/workspace"),
        ),
        (
            Cow::Borrowed("/workspace/removable"),
            Cow::Borrowed("/removable"),
        ),
    ];
    let inspected = PLUGIN_TREES
        .iter()
        .filter(|_| inspect)
        .map(|&(point, source)| {
            (
                Cow::Borrowed(point),
                Cow::Borrowed(source),
                MountOptions::read_only(),
            )
        });
    vfs.scoped_mounts_with(
        private
            .into_iter()
            .map(|(point, source)| (point, source, MountOptions::read_write()))
            .chain(inspected),
    )
}
