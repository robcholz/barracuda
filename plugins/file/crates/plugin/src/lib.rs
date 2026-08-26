//! System filesystem capability and dynamic RPC harness.

#![no_std]

extern crate alloc;

mod rpc;

use alloc::rc::Rc;

use barracuda_event_router::{
    Component, ComponentFuture, ComponentResult, RegisterContext, RunContext, UnregisterContext,
};
use barracuda_plugin_manager::{Plugin, PluginContext, PluginResult};
use barracuda_vfs::{FsError, Metadata, ScopedVfs};

pub use rpc::{
    FileBytes, FilePath, FileRead, FileReadRequest, FileRpcError, FileWrite, FileWriteRequest,
};

/// Stable identity of the File Plugin.
pub const PLUGIN_ID: &str = "file";

/// Cloneable API for files in the System-owned `/system` namespace.
#[derive(Clone)]
pub struct FileSystem {
    filesystem: ScopedVfs,
}

impl FileSystem {
    /// Creates a capability over a System-scoped VFS view.
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

    /// Renames a path within the System filesystem.
    pub async fn rename(&self, from: &str, to: &str) -> Result<(), FsError> {
        self.filesystem.rename(from, to).await
    }
}

/// Plugin that publishes [`FileSystem`] and exposes its RPC harness.
pub struct FilePlugin {
    filesystem: ScopedVfs,
}

impl FilePlugin {
    /// Creates the Plugin with a view rooted at `/system`.
    #[must_use]
    pub const fn new(filesystem: ScopedVfs) -> Self {
        Self { filesystem }
    }
}

impl<const M: usize> Plugin<M> for FilePlugin {
    fn id(&self) -> &'static str {
        PLUGIN_ID
    }

    fn register<Storage>(&mut self, context: &mut PluginContext<'_, M, Storage>) -> PluginResult<()>
    where
        Storage: barracuda_plugin_manager::PluginStorage,
    {
        let filesystem = Rc::new(FileSystem::new(self.filesystem.clone()));
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
