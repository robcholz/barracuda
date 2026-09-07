use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::future::Future as _;
use core::task::Poll;

use barracuda_lua::{Error as LuaError, ErrorKind as LuaErrorKind, LuaExecution};
use barracuda_vm_builtin_packages::{
    BuiltinPackages,
    io::{Input as LuaInput, Output as LuaOutput},
};
use barracuda_vm_package_api::LuaPackageRegistry;
use futures_channel::oneshot;

use crate::memory::VmMemoryLease;
use crate::runtime::{RunControl, VM_YIELD_DELAY_MILLIS, VmYieldSignal};
use crate::{VmExecutionError, VmLimits, VmRunCompletion, VmRunOutcome};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ExecutionErrorKind {
    VmCreate,
    VmConfigure,
    LuaLoad,
    LuaRuntime,
    UnexpectedYield,
    LuaMemory,
}

impl ExecutionErrorKind {
    const fn public(self) -> VmExecutionError {
        match self {
            Self::VmCreate => VmExecutionError::VmCreate,
            Self::VmConfigure => VmExecutionError::VmConfigure,
            Self::LuaLoad => VmExecutionError::LuaLoad,
            Self::LuaRuntime => VmExecutionError::LuaRuntime,
            Self::UnexpectedYield => VmExecutionError::UnexpectedYield,
            Self::LuaMemory => VmExecutionError::LuaMemory,
        }
    }
}

struct ExecutionError {
    kind: ExecutionErrorKind,
    diagnostic: String,
}

impl ExecutionError {
    fn new(kind: ExecutionErrorKind, diagnostic: &str) -> Self {
        Self {
            kind,
            diagnostic: diagnostic.to_string(),
        }
    }
}

pub(crate) struct ExecutionJob {
    pub(crate) run_id: u32,
    pub(crate) source: String,
    pub(crate) control: RunControl,
    pub(crate) memory: VmMemoryLease,
    pub(crate) limits: VmLimits,
    pub(crate) builtin_packages: BuiltinPackages,
    pub(crate) package_registry: LuaPackageRegistry,
    pub(crate) completion: oneshot::Sender<VmRunCompletion>,
}

pub(crate) async fn execute_run(job: ExecutionJob) {
    let ExecutionJob {
        run_id,
        source,
        control,
        memory,
        limits,
        builtin_packages,
        package_registry,
        completion,
    } = job;
    let mut output = Vec::new();
    let result = drive_execution(
        &mut output,
        ExecutionSetup {
            source,
            control: &control,
            limits,
            builtin_packages,
            package_registry,
            yield_signal: VmYieldSignal::default(),
            memory: &memory,
        },
    )
    .await;
    let result = if control.is_cancelled() {
        VmRunCompletion {
            run_id,
            outcome: VmRunOutcome::Cancelled,
            output,
            error: None,
            diagnostic: None,
        }
    } else {
        match result {
            Ok(()) => VmRunCompletion {
                run_id,
                outcome: VmRunOutcome::Success,
                output,
                error: None,
                diagnostic: None,
            },
            Err(error) => VmRunCompletion {
                run_id,
                outcome: VmRunOutcome::Error,
                output,
                error: Some(error.kind.public()),
                diagnostic: Some(error.diagnostic),
            },
        }
    };
    let _ignored = completion.send(result);
}

struct ExecutionSetup<'a> {
    source: String,
    control: &'a RunControl,
    limits: VmLimits,
    builtin_packages: BuiltinPackages,
    package_registry: LuaPackageRegistry,
    yield_signal: VmYieldSignal,
    memory: &'a VmMemoryLease,
}

async fn drive_execution(
    output_messages: &mut Vec<String>,
    setup: ExecutionSetup<'_>,
) -> Result<(), ExecutionError> {
    let mut lua = unsafe { setup.memory.create_lua() }.map_err(|error| factory_error(&error))?;
    let installed = setup
        .builtin_packages
        .install(&mut lua)
        .map_err(|error| ExecutionError::new(ExecutionErrorKind::VmConfigure, error.message()))?;
    let (lua_input, mut output) = installed.into_io();
    setup
        .package_registry
        .install(&mut lua)
        .map_err(|error| ExecutionError::new(ExecutionErrorKind::VmConfigure, error.message()))?;
    lua.set_instruction_hook(setup.limits.instruction_hook_interval(), {
        let yield_signal = setup.yield_signal.clone();
        move || yield_signal.mark()
    })
    .map_err(|error| ExecutionError::new(ExecutionErrorKind::VmConfigure, error.message()))?;

    let mut execution = lua.run(&setup.source);
    loop {
        match next_execution_event(
            &lua_input,
            &mut output,
            &mut execution,
            &setup.yield_signal,
            setup.control,
        )
        .await
        {
            ExecutionEvent::Output(Some(message)) => output_messages.push(message),
            ExecutionEvent::Output(None) => {}
            ExecutionEvent::InputRequired(true) => match setup.control.next_input().await {
                Some(input) => lua_input.send(input.as_str()).await.map_err(|error| {
                    ExecutionError::new(ExecutionErrorKind::LuaRuntime, error.message())
                })?,
                None if setup.control.is_cancelled() => return Ok(()),
                None => lua_input.close(),
            },
            ExecutionEvent::InputRequired(false) => lua_input.close(),
            ExecutionEvent::Complete(result) => {
                lua_input.close();
                while let Some(message) = output.next().await {
                    output_messages.push(message);
                }
                return result.map_err(|error| execution_error(&error));
            }
            ExecutionEvent::Yielded => {
                embassy_time::Timer::after_millis(VM_YIELD_DELAY_MILLIS).await;
            }
            ExecutionEvent::Cancelled => return Ok(()),
        }
    }
}

enum ExecutionEvent {
    Output(Option<String>),
    InputRequired(bool),
    Complete(barracuda_lua::Result<()>),
    Yielded,
    Cancelled,
}

async fn next_execution_event(
    input: &LuaInput,
    output: &mut LuaOutput,
    execution: &mut LuaExecution,
    yield_signal: &VmYieldSignal,
    control: &RunControl,
) -> ExecutionEvent {
    let mut next_output = core::pin::pin!(output.next());
    let mut next_input_request = core::pin::pin!(input.next_request());
    core::future::poll_fn(|context| {
        if control.is_cancelled() {
            return Poll::Ready(ExecutionEvent::Cancelled);
        }
        if let Poll::Ready(message) = next_output.as_mut().poll(context) {
            return Poll::Ready(ExecutionEvent::Output(message));
        }
        if let Poll::Ready(requested) = next_input_request.as_mut().poll(context) {
            return Poll::Ready(ExecutionEvent::InputRequired(requested));
        }
        match core::pin::Pin::new(&mut *execution).poll(context) {
            Poll::Ready(result) => Poll::Ready(ExecutionEvent::Complete(result)),
            Poll::Pending if yield_signal.take() => Poll::Ready(ExecutionEvent::Yielded),
            Poll::Pending => Poll::Pending,
        }
    })
    .await
}

fn factory_error(error: &LuaError) -> ExecutionError {
    let kind = match error.kind() {
        LuaErrorKind::Create => ExecutionErrorKind::VmCreate,
        LuaErrorKind::Memory => ExecutionErrorKind::LuaMemory,
        LuaErrorKind::Load
        | LuaErrorKind::Runtime
        | LuaErrorKind::Conversion
        | LuaErrorKind::UnexpectedYield => ExecutionErrorKind::VmConfigure,
    };
    ExecutionError::new(kind, error.message())
}

fn execution_error(error: &LuaError) -> ExecutionError {
    let kind = match error.kind() {
        LuaErrorKind::Load => ExecutionErrorKind::LuaLoad,
        LuaErrorKind::Memory => ExecutionErrorKind::LuaMemory,
        LuaErrorKind::UnexpectedYield => ExecutionErrorKind::UnexpectedYield,
        LuaErrorKind::Create | LuaErrorKind::Runtime | LuaErrorKind::Conversion => {
            ExecutionErrorKind::LuaRuntime
        }
    };
    ExecutionError::new(kind, error.message())
}
