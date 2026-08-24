//! Lua execution as one bidirectional Event Router RPC.
//!
//! Each `vm.run` call creates one isolated, crate-configured Lua state, streams
//! `io.input()` messages into it, and streams captured `io.print(...)` messages back
//! to the caller.
#![no_std]

extern crate alloc;

/// Event Router lifecycle integration.
pub mod component;
#[allow(unsafe_code)]
#[allow(clippy::indexing_slicing)]
mod memory;
/// The `vm.run` RPC contract and reusable handler.
#[allow(unsafe_code)]
pub mod run;
mod runtime;

pub use barracuda_vm_builtin_packages::BuiltinPackages;
pub use component::{VmComponent, VmLimits};
pub use memory::VmMemoryPoolError;
pub use runtime::{
    VM_MEMORY_BYTES_PER_SLOT, VM_TASK_SLOTS, VM_YIELD_DELAY_MILLIS, VmRuntime, VmRuntimeStartError,
};
