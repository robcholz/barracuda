//! Plugin-facing restricted VFS namespaces.

/// Cloneable file access rooted inside one Plugin's private namespace.
///
/// [`barracuda_vfs::ScopedVfs`] intentionally has no mount-table operations, so
/// the Plugin receives the complete file API without receiving VFS ownership.
pub type PluginVfs = barracuda_vfs::ScopedVfs;
