//! Plugin management and scoped persistent storage.
//!
//! A Plugin is a system-managed group of Components. Each Plugin receives one
//! durable storage capability whose keys are isolated from every other Plugin.
//! The Plugin decides what to store and which of its Components share access.

#![no_std]

extern crate alloc;

mod lifecycle;
mod storage;

pub use ekv::config::{
    ALIGN as EKV_ALIGN, ERASE_VALUE as EKV_ERASE_VALUE, MAX_PAGE_COUNT as EKV_MAX_PAGE_COUNT,
    PAGE_SIZE as EKV_PAGE_SIZE,
};
pub use ekv::flash::{Flash as EkvFlash, PageID as EkvPageId};
pub use ekv::Config as EkvConfig;
pub use embassy_sync_06::blocking_mutex::raw::{CriticalSectionRawMutex, NoopRawMutex, RawMutex};
pub use lifecycle::{
    CapabilityError, Plugin, PluginComponentCleanupFailure, PluginContext, PluginError, PluginId,
    PluginIdError, PluginManager, PluginRegisterError, PluginRegisterFuture, PluginResult,
    PluginStartError, PluginStartFuture, PluginUnloadError,
};
pub use storage::{EkvStore, ScopedStorage, StorageError, StorageMutation, StorageResult};
