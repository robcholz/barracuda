//! System filesystem capability and dynamic RPC harness.

#![no_std]

extern crate alloc;

mod rpc;

use alloc::rc::Rc;

use barracuda_event_router::{
    Component, ComponentFuture, ComponentResult, RegisterContext, RunContext, UnregisterContext,
};
use barracuda_plugin_api::PluginContext;
use barracuda_plugin_manager::{
    Plugin, PluginFilesystem, PluginRegisterContext, PluginRequirements, PluginResult,
};
use barracuda_vfs::{FsError, Metadata, ScopedVfs};

pub use rpc::{
    FileBytes, FilePath, FileRead, FileReadRequest, FileRpcError, FileWrite, FileWriteRequest,
};

/// Cloneable API for files in the File Plugin's private namespace.
#[derive(Clone)]
pub struct FileSystem {
    filesystem: ScopedVfs,
}

impl FileSystem {
    /// Creates a capability over a Plugin-scoped VFS view.
    #[must_use]
    pub const fn new(filesystem: ScopedVfs) -> Self {
        Self { filesystem }
    }

    /// Reads an entire file.
    pub async fn read(&self, path: &str) -> Result<alloc::vec::Vec<u8>, FsError> {
        self.filesystem.read(path).await
    }

    /// Replaces a file and creates missing parent directories.
    pub async fn write(&self, path: &str, bytes: &[u8]) -> Result<(), FsError> {
        self.filesystem.write(path, bytes).await
    }

    /// Lists immediate child names.
    pub async fn list(
        &self,
        path: &str,
    ) -> Result<alloc::vec::Vec<alloc::string::String>, FsError> {
        self.filesystem.list_dir(path).await
    }

    /// Creates a directory and all missing ancestors.
    pub async fn create_dir_all(&self, path: &str) -> Result<(), FsError> {
        self.filesystem.create_dir_all(path).await
    }

    /// Returns metadata for a path.
    pub async fn metadata(&self, path: &str) -> Result<Metadata, FsError> {
        self.filesystem.metadata(path).await
    }

    /// Removes a file or empty directory, succeeding when absent.
    pub async fn remove(&self, path: &str) -> Result<(), FsError> {
        self.filesystem.remove(path).await
    }

    /// Renames a path within the File Plugin's private filesystem.
    pub async fn rename(&self, from: &str, to: &str) -> Result<(), FsError> {
        self.filesystem.rename(from, to).await
    }
}

/// Plugin that publishes [`FileSystem`] and exposes its RPC harness.
#[barracuda_plugin_api::plugin]
pub struct FilePlugin;

impl FilePlugin {
    /// Creates the Plugin from shared System construction resources.
    #[must_use]
    pub const fn new<Builtins, Io>(_context: &mut PluginContext<Builtins, Io>) -> Self {
        Self
    }
}

impl<const M: usize> Plugin<M> for FilePlugin {
    const REQUIREMENTS: PluginRequirements =
        PluginRequirements::new().with_filesystem(PluginFilesystem::Private);

    fn register<Storage>(
        &mut self,
        context: &mut PluginRegisterContext<'_, M, Storage>,
    ) -> PluginResult<()>
    where
        Storage: barracuda_plugin_manager::PluginStorage,
    {
        let filesystem = Rc::new(FileSystem::new(context.filesystem()?.clone()));
        context.provide(Rc::clone(&filesystem))?;
        context.event_router.load(FileComponent { filesystem })?;
        Ok(())
    }
}

struct FileComponent {
    filesystem: Rc<FileSystem>,
}

impl<const M: usize> Component<M> for FileComponent {
    fn register(&mut self, context: &mut RegisterContext<'_, M>) -> ComponentResult<()> {
        context.register_rpc::<FileRead, _>(rpc::read_handler(Rc::clone(&self.filesystem)))?;
        context.register_rpc::<FileWrite, _>(rpc::write_handler(Rc::clone(&self.filesystem)))
    }

    fn run<'a>(&'a mut self, _context: RunContext<M>) -> ComponentFuture<'a> {
        alloc::boxed::Box::pin(core::future::pending())
    }

    fn unregister(&mut self, _context: &mut UnregisterContext<'_>) -> ComponentResult<()> {
        Ok(())
    }
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;
    use alloc::{string::String, vec};
    use barracuda_platform_test::memory_vfs;
    use futures_lite::future::block_on;

    #[test]
    fn filesystem_capability_owns_complete_namespace_mutations() {
        block_on(async {
            let filesystem = FileSystem::new(memory_vfs().await.expect("memory filesystem mounts"));

            filesystem
                .create_dir_all("/documents/archive")
                .await
                .expect("nested directory creates");
            filesystem
                .write("/documents/note.txt", b"hello")
                .await
                .expect("file writes");
            assert_eq!(
                filesystem.read("/documents/note.txt").await,
                Ok(b"hello".to_vec())
            );
            assert_eq!(
                filesystem.list("/documents").await,
                Ok(vec![String::from("archive"), String::from("note.txt")])
            );
            let metadata = filesystem
                .metadata("/documents/note.txt")
                .await
                .expect("metadata reads");
            assert!(metadata.is_file());
            assert_eq!(metadata.len(), 5);

            filesystem
                .rename("/documents/note.txt", "/documents/archive/note.txt")
                .await
                .expect("file renames");
            assert_eq!(
                filesystem.read("/documents/archive/note.txt").await,
                Ok(b"hello".to_vec())
            );
            assert_eq!(
                filesystem.read("/documents/note.txt").await,
                Err(FsError::NotFound)
            );
            filesystem
                .remove("/documents/archive/note.txt")
                .await
                .expect("file removes");
            filesystem
                .remove("/documents/archive/note.txt")
                .await
                .expect("removing an absent file is idempotent");
        });
    }
}
