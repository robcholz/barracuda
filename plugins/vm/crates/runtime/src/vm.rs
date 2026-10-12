use alloc::rc::{Rc, Weak};
use alloc::string::String;
use alloc::vec::Vec;
use core::cell::RefCell;
use core::future::Future;
use core::pin::Pin;
use core::task::{Context, Poll, Waker};

use barracuda_runtime_utils::oneshot;
use barracuda_vm_builtin_packages::BuiltinPackages;
pub use barracuda_vm_builtin_packages::math::SeedSource;
use barracuda_vm_package_api::LuaPackageRegistry;
use embassy_executor::Spawner;
use getset::CopyGetters;
use serde::{Deserialize, Serialize};

use crate::runtime::{ControlError, DispatchError, RunCancellation};
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

/// Terminal state of one accepted Lua execution.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum VmRunOutcome {
    /// The source completed successfully.
    Success,
    /// The caller cancelled the execution.
    Cancelled,
    /// Lua creation, configuration, loading, or execution failed.
    Error,
}

/// Stable execution failure reported after a run was accepted.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum VmExecutionError {
    /// The Lua state could not be created.
    VmCreate,
    /// The sandbox or native packages could not be configured.
    VmConfigure,
    /// The source document could not be loaded.
    LuaLoad,
    /// The loaded source failed while running.
    LuaRuntime,
    /// Lua yielded for a reason not owned by the VM scheduler.
    UnexpectedYield,
    /// The fixed Lua heap was exhausted.
    LuaMemory,
}

/// Complete result of one accepted Lua execution.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct VmRunCompletion {
    /// Runtime-generated execution identifier.
    pub run_id: u32,
    /// Terminal execution state.
    pub outcome: VmRunOutcome,
    /// Lines emitted through virtual standard output and error, in order.
    pub output: Vec<String>,
    /// Stable execution failure when `outcome` is `error`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<VmExecutionError>,
    /// Human-readable execution diagnostic when `outcome` is `error`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub diagnostic: Option<String>,
}

/// Current state of one active Lua execution.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum VmRunState {
    /// The execution is running or cooperatively yielding.
    Running,
    /// The execution requires input or EOF before it can continue.
    InputRequired,
}

/// Bounded public information about one active Lua execution.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct VmRunInfo {
    /// Runtime-generated execution identifier.
    pub run_id: u32,
    /// Current execution state.
    pub state: VmRunState,
}

/// Snapshot of all active Lua executions.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct VmListResponse {
    /// Active executions in runtime slot order.
    pub runs: Vec<VmRunInfo>,
}

/// Non-terminal state change produced by an accepted Lua execution.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum VmRunProgress {
    /// The execution is suspended until input or EOF is supplied.
    InputRequired {
        /// Runtime-generated execution identifier.
        run_id: u32,
    },
}

/// Next observable update from an accepted Lua execution.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(untagged)]
pub enum VmRunUpdate {
    /// The execution remains active.
    Progress(VmRunProgress),
    /// The execution has returned and cannot produce more updates.
    Completed(VmRunCompletion),
}

/// Awaitable handle for one accepted Lua execution.
#[derive(CopyGetters)]
pub struct VmRun {
    /// Runtime-generated execution identifier available before completion.
    #[getset(get_copy = "pub")]
    run_id: u32,
    progress: VmProgressReceiver,
    completion: oneshot::Receiver<VmRunCompletion>,
    cancellation: RunCancellation,
}

impl VmRun {
    pub(crate) const fn new(
        run_id: u32,
        progress: VmProgressReceiver,
        completion: oneshot::Receiver<VmRunCompletion>,
        cancellation: RunCancellation,
    ) -> Self {
        Self {
            run_id,
            progress,
            completion,
            cancellation,
        }
    }

    /// Waits for the next progress update or terminal completion.
    pub async fn next_update(&mut self) -> Result<VmRunUpdate, VmError> {
        core::future::poll_fn(|context| {
            if let Poll::Ready(progress) = self.progress.poll_next(context) {
                return Poll::Ready(Ok(VmRunUpdate::Progress(progress)));
            }
            Pin::new(&mut self.completion).poll(context).map(|result| {
                result
                    .map(VmRunUpdate::Completed)
                    .map_err(|_error| VmError::RuntimeUnavailable)
            })
        })
        .await
    }
}

impl Drop for VmRun {
    fn drop(&mut self) {
        self.cancellation.cancel();
    }
}

struct VmProgressState {
    next: RefCell<Option<VmRunProgress>>,
    waker: RefCell<Option<Waker>>,
}

pub(crate) struct VmProgressSender {
    state: Weak<VmProgressState>,
}

impl VmProgressSender {
    pub(crate) fn send(&self, progress: VmRunProgress) {
        let Some(state) = self.state.upgrade() else {
            return;
        };
        *state.next.borrow_mut() = Some(progress);
        if let Some(waker) = state.waker.borrow_mut().take() {
            waker.wake();
        }
    }
}

pub(crate) struct VmProgressReceiver {
    state: Rc<VmProgressState>,
}

impl VmProgressReceiver {
    fn poll_next(&mut self, context: &mut Context<'_>) -> Poll<VmRunProgress> {
        if let Some(progress) = self.state.next.borrow_mut().take() {
            return Poll::Ready(progress);
        }
        *self.state.waker.borrow_mut() = Some(context.waker().clone());
        Poll::Pending
    }
}

pub(crate) fn vm_progress_channel() -> (VmProgressSender, VmProgressReceiver) {
    let state = Rc::new(VmProgressState {
        next: RefCell::new(None),
        waker: RefCell::new(None),
    });
    (
        VmProgressSender {
            state: Rc::downgrade(&state),
        },
        VmProgressReceiver { state },
    )
}

impl Future for VmRun {
    type Output = Result<VmRunCompletion, VmError>;

    fn poll(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Self::Output> {
        Pin::new(&mut self.completion)
            .poll(context)
            .map(|result| result.map_err(|_error| VmError::RuntimeUnavailable))
    }
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
    /// Creates the VM capability with its fixed execution pool, seeding each
    /// execution's `math.random` from `seeds`.
    pub fn new(
        package_registry: LuaPackageRegistry,
        seeds: SeedSource,
    ) -> Result<Self, VmMemoryPoolError> {
        Ok(Self {
            runtime: VmRuntime::new()?,
            limits: VmLimits::default(),
            builtin_packages: BuiltinPackages::new(seeds),
            package_registry,
        })
    }

    /// Installs the task spawner after Plugin registration completes.
    pub fn start(&self, spawner: Spawner) -> Result<(), VmRuntimeStartError> {
        self.runtime.start(spawner)
    }

    /// Stops accepting executions and cancels every active run.
    pub fn stop(&self) {
        self.runtime.stop();
    }

    /// Starts one isolated Lua execution and returns its awaitable completion handle.
    pub fn run(&self, request: VmRunRequest) -> Result<VmRun, VmError> {
        self.runtime
            .dispatch(
                request.source,
                self.limits,
                self.builtin_packages.clone(),
                self.package_registry.clone(),
            )
            .map_err(VmError::from)
    }

    /// Returns a snapshot of all currently active executions.
    #[must_use]
    pub fn list(&self) -> VmListResponse {
        VmListResponse {
            runs: self.runtime.list(),
        }
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
