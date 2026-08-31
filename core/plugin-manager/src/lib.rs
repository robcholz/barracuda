//! Plugin management and scoped persistent storage.
//!
//! A Plugin is a system-managed group of Components. Each Plugin receives one
//! durable storage capability whose keys are isolated from every other Plugin.
//! The Plugin decides what to store and which of its Components share access.

#![no_std]

extern crate alloc;

mod filesystem;
mod lifecycle;
mod storage;

pub use barracuda_kv::Value;
pub use filesystem::PluginVfs;
pub use lifecycle::{
    CapabilityError, Plugin, PluginComponentCleanupFailure, PluginDeclaration, PluginError,
    PluginEventRouterContext, PluginFilesystem, PluginId, PluginIdError, PluginManager,
    PluginManagerInitError, PluginRegisterContext, PluginRegisterError, PluginRequirements,
    PluginResult, PluginStartContext, PluginStartError, PluginTaskToken, PluginUnloadError,
};
pub use storage::{
    PluginReadTransaction, PluginStorage, PluginWriteTransaction, StorageError, StorageResult,
};
