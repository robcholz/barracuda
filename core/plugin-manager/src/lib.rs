//! Plugin management and scoped persistent storage.
//!
//! A Plugin is a system-managed group of Components. Each Plugin receives one
//! durable storage capability whose keys are isolated from every other Plugin.
//! The Plugin decides what to store and which of its Components share access.

#![no_std]

extern crate alloc;

mod lifecycle;
mod storage;

pub use barracuda_kv::Value;
pub use lifecycle::{
    CapabilityError, Plugin, PluginComponentCleanupFailure, PluginContext, PluginError, PluginId,
    PluginIdError, PluginManager, PluginManagerInitError, PluginRegisterError,
    PluginRegisterFuture, PluginResult, PluginStartError, PluginStartFuture, PluginUnloadError,
};
pub use storage::{
    PluginReadTransaction, PluginStorage, PluginWriteTransaction, StorageError, StorageResult,
};
