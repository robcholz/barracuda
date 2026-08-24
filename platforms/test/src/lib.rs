//! Deterministic Platform capabilities for unit and integration tests.

#![no_std]

extern crate alloc;

mod filesystem;
mod flash;
mod network;

pub use filesystem::{MemFile, MemFs};
pub use flash::{
    memory_partition, MemoryNorFlash, MemoryNorFlashError, MemoryPartition, MemoryPartitionError,
};
pub use network::{
    NeverConnection, NeverStack, NeverUdp, ScriptError, ScriptStep, ScriptedConnection,
    ScriptedStack,
};
