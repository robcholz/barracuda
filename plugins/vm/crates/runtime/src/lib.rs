//! Isolated Lua execution capability with awaitable completion.
#![no_std]

extern crate alloc;

#[allow(unsafe_code)]
#[allow(clippy::indexing_slicing)]
mod memory;
#[allow(unsafe_code)]
mod run;
mod runtime;
mod vm;

pub use barracuda_vm_builtin_packages::BuiltinPackages;
pub use memory::VmMemoryPoolError;
pub use runtime::{VmRuntime, VmRuntimeStartError};
pub use vm::{
    Vm, VmControlAccepted, VmError, VmExecutionError, VmInputRequest, VmLimits, VmRun,
    VmRunCompletion, VmRunOutcome, VmRunReference, VmRunRequest,
};
