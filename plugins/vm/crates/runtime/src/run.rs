use alloc::rc::Rc;
use alloc::string::{String, ToString};
use core::future::Future as _;
use core::task::Poll;

use barracuda_lua::{Error as LuaError, ErrorKind as LuaErrorKind, LuaExecution};
use barracuda_vm_builtin_packages::{
    BuiltinPackages,
    io::{Input as LuaInput, Output as LuaOutput},
};
use barracuda_vm_package_api::LuaPackageRegistry;
use barracuda_workflow_plugin::{Event, WorkflowService};
use serde::Serialize;

use crate::VmLimits;
use crate::memory::VmMemoryLease;
use crate::runtime::{RunControl, VM_YIELD_DELAY_MILLIS, VmYieldSignal};

/// One complete output message emitted by an active execution.
pub struct Output;

impl Event for Output {
    const ID: &'static str = "vm.output";
}

/// Notification that an execution is blocked in `io.input()`.
pub struct InputRequired;

impl Event for InputRequired {
    const ID: &'static str = "vm.input_required";
}

/// Terminal outcome for one execution.
pub struct Finished;

impl Event for Finished {
    const ID: &'static str = "vm.finished";
}

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
    const fn code(self) -> &'static str {
        match self {
            Self::VmCreate => "vm_create",
            Self::VmConfigure => "vm_configure",
            Self::LuaLoad => "lua_load",
            Self::LuaRuntime => "lua_runtime",
            Self::UnexpectedYield => "unexpected_yield",
            Self::LuaMemory => "lua_memory",
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
    pub(crate) workflow: Rc<WorkflowService>,
    pub(crate) run_id: u32,
    pub(crate) source: String,
    pub(crate) control: RunControl,
    pub(crate) memory: VmMemoryLease,
    pub(crate) limits: VmLimits,
    pub(crate) builtin_packages: BuiltinPackages,
    pub(crate) package_registry: LuaPackageRegistry,
}

pub(crate) async fn execute_run(job: ExecutionJob) {
    let ExecutionJob {
        workflow,
        run_id,
        source,
        control,
        memory,
        limits,
        builtin_packages,
        package_registry,
    } = job;
    let mut events = RunEvents::new(workflow, run_id);
    let result = drive_execution(
        &mut events,
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
    let terminal = if control.is_cancelled() {
        TerminalOutcome::Cancelled
    } else {
        match result {
            Ok(()) => TerminalOutcome::Success,
            Err(error) => TerminalOutcome::Error(error),
        }
    };
    let _result = events.finished(terminal);
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
    events: &mut RunEvents,
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
            ExecutionEvent::Output(Some(message)) => events.output(&message)?,
            ExecutionEvent::Output(None) => {}
            ExecutionEvent::InputRequired(true) => {
                events.input_required()?;
                match setup.control.next_input().await {
                    Some(input) => lua_input.send(input.as_str()).await.map_err(|error| {
                        ExecutionError::new(ExecutionErrorKind::LuaRuntime, error.message())
                    })?,
                    None if setup.control.is_cancelled() => return Ok(()),
                    None => lua_input.close(),
                }
            }
            ExecutionEvent::InputRequired(false) => lua_input.close(),
            ExecutionEvent::Complete(result) => {
                lua_input.close();
                while let Some(message) = output.next().await {
                    events.output(&message)?;
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

struct RunEvents {
    workflow: Rc<WorkflowService>,
    run_id: u32,
    sequence: u64,
}

impl RunEvents {
    fn new(workflow: Rc<WorkflowService>, run_id: u32) -> Self {
        Self {
            workflow,
            run_id,
            sequence: 0,
        }
    }

    fn output(&mut self, message: &str) -> Result<(), ExecutionError> {
        self.emit::<Output, _>(&OutputFields {
            run_id: self.run_id,
            sequence: self.sequence,
            chunk: message,
            message_end: true,
        })?;
        self.advance_sequence()
    }

    fn input_required(&mut self) -> Result<(), ExecutionError> {
        self.emit::<InputRequired, _>(&OrderedFields {
            run_id: self.run_id,
            sequence: self.sequence,
        })?;
        self.advance_sequence()
    }

    fn finished(&mut self, outcome: TerminalOutcome) -> Result<(), ExecutionError> {
        let (outcome, error, diagnostic) = match &outcome {
            TerminalOutcome::Success => ("success", None, None),
            TerminalOutcome::Cancelled => ("cancelled", None, None),
            TerminalOutcome::Error(error) => (
                "error",
                Some(error.kind.code()),
                Some(error.diagnostic.as_str()),
            ),
        };
        self.emit::<Finished, _>(&FinishedFields {
            run_id: self.run_id,
            sequence: self.sequence,
            outcome,
            error,
            diagnostic,
        })
    }

    fn emit<E, Payload>(&self, payload: &Payload) -> Result<(), ExecutionError>
    where
        E: Event,
        Payload: Serialize,
    {
        let input = serde_json::to_value(payload).map_err(|_error| {
            ExecutionError::new(
                ExecutionErrorKind::VmConfigure,
                "Event serialization failed",
            )
        })?;
        self.workflow.emit::<E>(input).map_err(|_error| {
            ExecutionError::new(ExecutionErrorKind::VmConfigure, "Event delivery failed")
        })
    }

    fn advance_sequence(&mut self) -> Result<(), ExecutionError> {
        self.sequence = self.sequence.checked_add(1).ok_or_else(|| {
            ExecutionError::new(
                ExecutionErrorKind::VmConfigure,
                "VM event sequence overflow",
            )
        })?;
        Ok(())
    }
}

#[derive(Serialize)]
struct OrderedFields {
    run_id: u32,
    sequence: u64,
}

#[derive(Serialize)]
struct OutputFields<'a> {
    run_id: u32,
    sequence: u64,
    chunk: &'a str,
    message_end: bool,
}

enum TerminalOutcome {
    Success,
    Cancelled,
    Error(ExecutionError),
}

#[derive(Serialize)]
struct FinishedFields<'a> {
    run_id: u32,
    sequence: u64,
    outcome: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    diagnostic: Option<&'a str>,
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
