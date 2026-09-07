//! Isolated Lua execution capability with Workflow Events.
#![no_std]

extern crate alloc;

#[allow(unsafe_code)]
#[allow(clippy::indexing_slicing)]
mod memory;
/// VM Workflow Event identities.
#[allow(unsafe_code)]
pub mod run;
mod runtime;
mod vm;

pub use barracuda_vm_builtin_packages::BuiltinPackages;
pub use memory::VmMemoryPoolError;
pub use runtime::{VmRuntime, VmRuntimeStartError};
pub use vm::{
    Vm, VmControlAccepted, VmError, VmInputRequest, VmLimits, VmRunAccepted, VmRunReference,
    VmRunRequest,
};
