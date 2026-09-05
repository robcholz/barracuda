//! Isolated Lua execution exposed through bounded JSON RPCs and JSON Events.
#![no_std]

extern crate alloc;

/// Event Router lifecycle integration.
pub mod component;
#[allow(unsafe_code)]
#[allow(clippy::indexing_slicing)]
mod memory;
/// VM JSON RPC, Event, and execution contracts.
#[allow(unsafe_code)]
pub mod run;
mod runtime;

pub use barracuda_vm_builtin_packages::BuiltinPackages;
pub use component::{VmComponent, VmLimits};
pub use memory::VmMemoryPoolError;
pub use runtime::{VmRuntime, VmRuntimeStartError};
