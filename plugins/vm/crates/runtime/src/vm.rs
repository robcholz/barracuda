use alloc::rc::Rc;
use alloc::string::String;

use barracuda_vm_builtin_packages::BuiltinPackages;
use barracuda_vm_package_api::LuaPackageRegistry;
use barracuda_workflow_plugin::WorkflowService;
use embassy_executor::Spawner;
use getset::CopyGetters;
use serde::{Deserialize, Serialize};

use crate::runtime::{ControlError, DispatchError};
use crate::{VmMemoryPoolError, VmRuntime, VmRuntimeStartError};

/// Default instruction interval between cooperative executor yields.
pub const DEFAULT_INSTRUCTION_HOOK_INTERVAL: u32 = 10_000;

/// Lua execution limits independent of any transport framing.
#[derive(Clone, Copy, Debug, Eq, PartialEq, CopyGetters)]
pub struct VmLimits {
    /// Lua instruction count between cooperative executor yields.
    #[getset(get_copy = "pub")]
    instruction_hook_interval: u32,
}

impl VmLimits {
    /// Creates limits with an explicit cooperative-yield interval.
    #[must_use]
    pub const fn new(instruction_hook_interval: u32) -> Self {
        Self {
            instruction_hook_interval,
        }
    }
}

impl Default for VmLimits {
    fn default() -> Self {
        Self::new(DEFAULT_INSTRUCTION_HOOK_INTERVAL)
    }
}

/// Request to start one isolated Lua execution.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct VmRunRequest {
    /// Complete Lua source document.
    pub source: String,
}

/// Accepted VM execution.
#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
pub struct VmRunAccepted {
    /// Runtime-generated execution identifier.
    pub run_id: u32,
}

/// Request to provide input or EOF to an active execution.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct VmInputRequest {
    /// Execution identifier returned by [`Vm::run`].
    pub run_id: u32,
    /// One complete input value.
    pub input: Option<String>,
    /// Set to `true` instead of `input` to close the input stream.
    pub eof: Option<bool>,
}

/// Reference to one active execution.
#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct VmRunReference {
    /// Execution identifier returned by [`Vm::run`].
    pub run_id: u32,
}

/// Empty successful control response.
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize)]
pub struct VmControlAccepted {}

/// Business rejection returned by the VM capability.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, thiserror::Error)]
#[serde(rename_all = "snake_case")]
pub enum VmError {
    /// The runtime has not started yet.
    #[error("VM runtime is unavailable")]
    RuntimeUnavailable,
    /// All execution slots are occupied.
    #[error("VM runtime is busy")]
    Busy,
    /// The referenced execution does not exist.
    #[error("VM execution was not found")]
    RunNotFound,
    /// One input value is already waiting to be consumed.
    #[error("VM input queue is full")]
    InputBackpressure,
    /// The execution no longer accepts input.
    #[error("VM input is closed")]
    InputClosed,
    /// Exactly one of `input` or `eof: true` must be supplied.
    #[error("invalid VM input request")]
    InvalidInput,
}

impl From<DispatchError> for VmError {
    fn from(error: DispatchError) -> Self {
        match error {
            DispatchError::RuntimeUnavailable => Self::RuntimeUnavailable,
            DispatchError::Busy => Self::Busy,
        }
    }
}

impl From<ControlError> for VmError {
    fn from(error: ControlError) -> Self {
        match error {
            ControlError::RunNotFound => Self::RunNotFound,
            ControlError::InputBackpressure => Self::InputBackpressure,
            ControlError::InputClosed => Self::InputClosed,
        }
    }
}

/// Typed capability for starting and controlling isolated Lua executions.
pub struct Vm {
    runtime: VmRuntime,
    limits: VmLimits,
    builtin_packages: BuiltinPackages,
    package_registry: LuaPackageRegistry,
}

impl Vm {
    /// Creates the VM capability with its fixed execution pool.
    pub fn new(package_registry: LuaPackageRegistry) -> Result<Self, VmMemoryPoolError> {
        Ok(Self {
            runtime: VmRuntime::new()?,
            limits: VmLimits::default(),
            builtin_packages: BuiltinPackages::all(),
            package_registry,
        })
    }

    /// Replaces Lua execution limits.
    #[must_use]
    pub fn with_limits(mut self, limits: VmLimits) -> Self {
        self.limits = limits;
        self
    }

    /// Installs the task spawner and Workflow Event destination.
    pub fn start(
        &self,
        spawner: Spawner,
        workflow: Rc<WorkflowService>,
    ) -> Result<(), VmRuntimeStartError> {
        self.runtime.start(spawner, workflow)
    }

    /// Stops accepting executions and cancels every active run.
    pub fn stop(&self) {
        self.runtime.stop();
    }

    /// Starts one isolated Lua execution and returns immediately after acceptance.
    pub fn run(&self, request: VmRunRequest) -> Result<VmRunAccepted, VmError> {
        self.runtime
            .dispatch(
                request.source,
                self.limits,
                self.builtin_packages,
                self.package_registry.clone(),
            )
            .map(|run_id| VmRunAccepted { run_id })
            .map_err(VmError::from)
    }

    /// Supplies one input value or EOF to an active execution.
    pub fn input(&self, request: VmInputRequest) -> Result<VmControlAccepted, VmError> {
        let result = match (request.input, request.eof) {
            (Some(input), None | Some(false)) => self.runtime.send_input(request.run_id, input),
            (None, Some(true)) => self.runtime.close_input(request.run_id),
            _ => return Err(VmError::InvalidInput),
        };
        result.map_err(VmError::from)?;
        Ok(VmControlAccepted {})
    }

    /// Cancels one active execution.
    pub fn cancel(&self, request: VmRunReference) -> Result<VmControlAccepted, VmError> {
        self.runtime.cancel(request.run_id).map_err(VmError::from)?;
        Ok(VmControlAccepted {})
    }
}
