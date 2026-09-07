//! Plugin management and scoped persistent storage.
//!
//! A Plugin is a system-managed capability and task owner. Each Plugin receives
//! one durable storage capability whose keys are isolated from every other
//! Plugin.

#![no_std]

extern crate alloc;

mod filesystem;
mod lifecycle;
mod storage;

pub use barracuda_kv::{Value, WriteValue};
pub use lifecycle::{
    CapabilityError, Plugin, PluginDeclaration, PluginError, PluginFilesystem, PluginId,
    PluginIdError, PluginManager, PluginManagerInitError, PluginRegisterContext,
    PluginRegisterError, PluginRequirements, PluginResult, PluginStartContext, PluginStartError,
    PluginTaskToken, PluginUnloadError,
};
pub use storage::{
    PluginEntry, PluginEntryIterator, PluginReadTransaction, PluginStorage, PluginWriteTransaction,
    StorageError, StorageResult,
};
