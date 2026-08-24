//! Deterministic Platform capabilities for unit and integration tests.

#![no_std]

extern crate alloc;
#[cfg(feature = "std")]
extern crate std;

mod flash;
mod network;
mod vfs;

pub use flash::{
    memory_partition, MemoryNorFlash, MemoryNorFlashError, MemoryPartition, MemoryPartitionError,
};
pub use network::{
    loopback_network, never_embassy_stack, LoopbackNetwork, NeverConnection, NeverStack, NeverUdp,
    ScriptError, ScriptStep, ScriptedConnection, ScriptedStack,
};
pub use vfs::{install_global_memory_vfs, memory_vfs, memory_vfs_root};
