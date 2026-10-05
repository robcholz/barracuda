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

pub use memory::VmMemoryPoolError;
#[cfg(feature = "test-fixture")]
pub use memory::{FixedMemoryLua, FixedMemoryLuaError};
pub use runtime::{VmRuntime, VmRuntimeStartError};
pub use vm::{
    Vm, VmControlAccepted, VmError, VmExecutionError, VmInputRequest, VmLimits, VmListResponse,
    VmRun, VmRunCompletion, VmRunInfo, VmRunOutcome, VmRunProgress, VmRunReference, VmRunRequest,
    VmRunState, VmRunUpdate,
};
