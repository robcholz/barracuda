use alloc::string::String;
use core::future::Future as _;
use core::task::Poll;

use barracuda_event_router::{
    Event, EventEmitter, JsonHandler, JsonPayload, JsonRef, JsonRpcSchema, JsonSchema, JsonWriter,
    RpcError, json_schema,
};
use barracuda_lua::{Error as LuaError, ErrorKind as LuaErrorKind, LuaExecution};
use barracuda_rpc::{JsonObjectFields, JsonObjectPayload, JsonObjectWriter};
use barracuda_vm_builtin_packages::{
    BuiltinPackages,
    io::{Input as LuaInput, Output as LuaOutput},
};
use barracuda_vm_package_api::LuaPackageRegistry;
use serde::Deserialize;

use crate::VmLimits;
use crate::component::DEFAULT_MAX_SOURCE_BYTES;
use crate::memory::VmMemoryLease;
use crate::runtime::{ControlError, RunControl, VM_YIELD_DELAY_MILLIS, VmRuntime, VmYieldSignal};

/// Maximum encoded JSON document accepted by VM control RPCs.
pub const VM_JSON_REQUEST_BYTES: usize = 512;
/// Maximum encoded JSON document returned by VM control RPCs.
pub const VM_JSON_RESPONSE_BYTES: usize = 48;
/// Maximum raw UTF-8 bytes carried by one output Event chunk.
pub const VM_OUTPUT_CHUNK_BYTES: usize = 48;
/// Maximum diagnostic bytes included in a terminal Event.
pub const VM_DIAGNOSTIC_BYTES: usize = 48;
/// Maximum encoded JSON input document emitted by any VM Event.
pub const VM_EVENT_INPUT_BYTES: usize = 416;

/// Starts one isolated Lua execution.
pub struct Run;

impl JsonRpcSchema for Run {
    const ADDRESS: &'static str = "vm.run";
    const REQUEST_SCHEMA: JsonSchema = json_schema!("run", request);
    const RESPONSE_SCHEMA: JsonSchema = json_schema!("run", response);
    const MAX_REQUEST_BYTES: usize = VM_JSON_REQUEST_BYTES;
    const MAX_RESPONSE_BYTES: usize = VM_JSON_RESPONSE_BYTES;
}

/// Supplies one complete input value, or EOF, to an active execution.
pub struct Input;

impl JsonRpcSchema for Input {
    const ADDRESS: &'static str = "vm.input";
    const REQUEST_SCHEMA: JsonSchema = json_schema!("input", request);
    const RESPONSE_SCHEMA: JsonSchema = json_schema!("input", response);
    const MAX_REQUEST_BYTES: usize = VM_JSON_REQUEST_BYTES;
    const MAX_RESPONSE_BYTES: usize = VM_JSON_RESPONSE_BYTES;
}

/// Cancels one active execution.
pub struct Cancel;

impl JsonRpcSchema for Cancel {
    const ADDRESS: &'static str = "vm.cancel";
    const REQUEST_SCHEMA: JsonSchema = json_schema!("cancel", request);
    const RESPONSE_SCHEMA: JsonSchema = json_schema!("cancel", response);
    const MAX_REQUEST_BYTES: usize = 32;
    const MAX_RESPONSE_BYTES: usize = VM_JSON_RESPONSE_BYTES;
}

/// One bounded output chunk emitted by an active execution.
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

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RunRequest<'a> {
    #[serde(borrow)]
    source: &'a str,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct InputRequest<'a> {
    run_id: u32,
    #[serde(borrow)]
    input: Option<&'a str>,
    eof: Option<bool>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RunReference {
    run_id: u32,
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
    diagnostic: Diagnostic,
}

impl ExecutionError {
    fn new(kind: ExecutionErrorKind, diagnostic: &str) -> Self {
        Self {
            kind,
            diagnostic: Diagnostic::new(diagnostic),
        }
    }
}

struct Diagnostic {
    bytes: [u8; VM_DIAGNOSTIC_BYTES],
    length: usize,
}

impl Diagnostic {
    fn new(diagnostic: &str) -> Self {
        let diagnostic = truncate_utf8(diagnostic, VM_DIAGNOSTIC_BYTES);
        let mut bytes = [0_u8; VM_DIAGNOSTIC_BYTES];
        if let Some(output) = bytes.get_mut(..diagnostic.len()) {
            output.copy_from_slice(diagnostic.as_bytes());
        }
        Self {
            bytes,
            length: diagnostic.len(),
        }
    }

    fn as_str(&self) -> &str {
        core::str::from_utf8(self.bytes.get(..self.length).unwrap_or_default()).unwrap_or_default()
    }
}

pub(crate) struct OwnedSource {
    bytes: [u8; DEFAULT_MAX_SOURCE_BYTES],
    length: usize,
}

impl OwnedSource {
    pub(crate) fn new(source: &str) -> Option<Self> {
        let mut bytes = [0_u8; DEFAULT_MAX_SOURCE_BYTES];
        bytes
            .get_mut(..source.len())?
            .copy_from_slice(source.as_bytes());
        Some(Self {
            bytes,
            length: source.len(),
        })
    }

    fn as_str(&self) -> &str {
        core::str::from_utf8(self.bytes.get(..self.length).unwrap_or_default()).unwrap_or_default()
    }
}

/// Builds the JSON handler for [`Run`].
pub fn run_handler(
    runtime: VmRuntime,
    limits: VmLimits,
    builtin_packages: BuiltinPackages,
    package_registry: LuaPackageRegistry,
) -> impl JsonHandler {
    move |_context, request: JsonRef, response: JsonWriter| {
        let runtime = runtime.clone();
        let package_registry = package_registry.clone();
        async move {
            let request = request.deserialize::<RunRequest<'_>>()?;
            if request.source.len() > limits.max_source_bytes() {
                return response
                    .write(&ErrorResponse("source_limit_exceeded"))
                    .await;
            }
            match runtime.dispatch(request.source, limits, builtin_packages, package_registry) {
                Ok(run_id) => response.write(&RunAccepted(run_id)).await,
                Err(error) => response.write(&ErrorResponse(error.code())).await,
            }
        }
    }
}

/// Builds the JSON handler for [`Input`].
pub fn input_handler(runtime: VmRuntime, limits: VmLimits) -> impl JsonHandler {
    move |_context, request: JsonRef, response: JsonWriter| {
        let runtime = runtime.clone();
        async move {
            let request = request.deserialize::<InputRequest<'_>>()?;
            let result = match (request.input, request.eof) {
                (Some(input), None | Some(false)) if input.len() <= limits.max_input_bytes() => {
                    runtime.send_input(request.run_id, input)
                }
                (Some(_input), None | Some(false)) => Err(ControlError::InputLimitExceeded),
                (None, Some(true)) => runtime.close_input(request.run_id),
                _ => return Err(RpcError::InvalidJson),
            };
            match result {
                Ok(()) => response.write("{}").await,
                Err(error) => response.write(&ErrorResponse(error.code())).await,
            }
        }
    }
}

/// Builds the JSON handler for [`Cancel`].
pub fn cancel_handler(runtime: VmRuntime) -> impl JsonHandler {
    move |_context, request: JsonRef, response: JsonWriter| {
        let runtime = runtime.clone();
        async move {
            let request = request.deserialize::<RunReference>()?;
            match runtime.cancel(request.run_id) {
                Ok(()) => response.write("{}").await,
                Err(error) => response.write(&ErrorResponse(error.code())).await,
            }
        }
    }
}

struct RunAccepted(u32);

impl JsonPayload for RunAccepted {
    fn encoded_len(&self) -> Result<usize, RpcError> {
        JsonObjectPayload::new(self).encoded_len()
    }

    fn write_json(&self, destination: &mut [u8]) -> Result<usize, RpcError> {
        JsonObjectPayload::new(self).write_json(destination)
    }
}

impl JsonObjectFields for RunAccepted {
    fn write_fields(&self, writer: &mut JsonObjectWriter<'_>) -> Result<(), RpcError> {
        writer.field("run_id", &JsonInteger(u64::from(self.0)))
    }
}

struct ErrorResponse(&'static str);

impl JsonPayload for ErrorResponse {
    fn encoded_len(&self) -> Result<usize, RpcError> {
        JsonObjectPayload::new(self).encoded_len()
    }

    fn write_json(&self, destination: &mut [u8]) -> Result<usize, RpcError> {
        JsonObjectPayload::new(self).write_json(destination)
    }
}

impl JsonObjectFields for ErrorResponse {
    fn write_fields(&self, writer: &mut JsonObjectWriter<'_>) -> Result<(), RpcError> {
        writer.string_field("error", self.0)
    }
}

pub(crate) struct ExecutionJob {
    pub(crate) emitter: EventEmitter<VM_JSON_REQUEST_BYTES>,
    pub(crate) run_id: u32,
    pub(crate) source: OwnedSource,
    pub(crate) control: RunControl,
    pub(crate) memory: VmMemoryLease,
    pub(crate) limits: VmLimits,
    pub(crate) builtin_packages: BuiltinPackages,
    pub(crate) package_registry: LuaPackageRegistry,
}

pub(crate) async fn execute_run(job: ExecutionJob) {
    let ExecutionJob {
        emitter,
        run_id,
        source,
        control,
        memory,
        limits,
        builtin_packages,
        package_registry,
    } = job;
    let mut events = RunEvents::new(emitter, run_id);
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
    let _result = events.finished(terminal).await;
}

struct ExecutionSetup<'a> {
    source: OwnedSource,
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

    let mut execution = start_execution(lua, setup.source);
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
            ExecutionEvent::Output(Some(message)) => events.output(&message).await?,
            ExecutionEvent::Output(None) => {}
            ExecutionEvent::InputRequired(true) => {
                events.input_required().await?;
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
                    events.output(&message).await?;
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

fn start_execution(lua: barracuda_lua::Lua, source: OwnedSource) -> LuaExecution {
    lua.run(source.as_str())
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
    emitter: EventEmitter<VM_JSON_REQUEST_BYTES>,
    run_id: u32,
    sequence: u64,
}

impl RunEvents {
    const fn new(emitter: EventEmitter<VM_JSON_REQUEST_BYTES>, run_id: u32) -> Self {
        Self {
            emitter,
            run_id,
            sequence: 0,
        }
    }

    async fn output(&mut self, message: &str) -> Result<(), ExecutionError> {
        if message.is_empty() {
            self.emit_output("", true).await?;
            return Ok(());
        }
        let mut remaining = message;
        while !remaining.is_empty() {
            let end = utf8_prefix(remaining, VM_OUTPUT_CHUNK_BYTES);
            let (chunk, rest) = remaining.split_at(end);
            self.emit_output(chunk, rest.is_empty()).await?;
            remaining = rest;
        }
        Ok(())
    }

    async fn emit_output(&mut self, chunk: &str, message_end: bool) -> Result<(), ExecutionError> {
        let fields = OutputFields {
            run_id: self.run_id,
            sequence: self.sequence,
            chunk,
            message_end,
        };
        self.emitter
            .emit::<Output>(&JsonObjectPayload::new(&fields))
            .await
            .map_err(|_error| {
                ExecutionError::new(ExecutionErrorKind::VmConfigure, "Event delivery failed")
            })?;
        self.advance_sequence()
    }

    async fn input_required(&mut self) -> Result<(), ExecutionError> {
        let fields = OrderedFields {
            run_id: self.run_id,
            sequence: self.sequence,
        };
        self.emitter
            .emit::<InputRequired>(&JsonObjectPayload::new(&fields))
            .await
            .map_err(|_error| {
                ExecutionError::new(ExecutionErrorKind::VmConfigure, "Event delivery failed")
            })?;
        self.advance_sequence()
    }

    async fn finished(&mut self, outcome: TerminalOutcome) -> Result<(), ExecutionError> {
        let fields = FinishedFields {
            run_id: self.run_id,
            sequence: self.sequence,
            outcome,
        };
        self.emitter
            .emit::<Finished>(&JsonObjectPayload::new(&fields))
            .await
            .map_err(|_error| {
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

struct OrderedFields {
    run_id: u32,
    sequence: u64,
}

impl JsonObjectFields for OrderedFields {
    fn write_fields(&self, writer: &mut JsonObjectWriter<'_>) -> Result<(), RpcError> {
        writer.field("run_id", &JsonInteger(u64::from(self.run_id)))?;
        writer.field("sequence", &JsonInteger(self.sequence))
    }
}

struct OutputFields<'a> {
    run_id: u32,
    sequence: u64,
    chunk: &'a str,
    message_end: bool,
}

impl JsonObjectFields for OutputFields<'_> {
    fn write_fields(&self, writer: &mut JsonObjectWriter<'_>) -> Result<(), RpcError> {
        writer.field("run_id", &JsonInteger(u64::from(self.run_id)))?;
        writer.field("sequence", &JsonInteger(self.sequence))?;
        writer.string_field("chunk", self.chunk)?;
        writer.field(
            "message_end",
            if self.message_end { "true" } else { "false" },
        )
    }
}

enum TerminalOutcome {
    Success,
    Cancelled,
    Error(ExecutionError),
}

struct FinishedFields {
    run_id: u32,
    sequence: u64,
    outcome: TerminalOutcome,
}

impl JsonObjectFields for FinishedFields {
    fn write_fields(&self, writer: &mut JsonObjectWriter<'_>) -> Result<(), RpcError> {
        writer.field("run_id", &JsonInteger(u64::from(self.run_id)))?;
        writer.field("sequence", &JsonInteger(self.sequence))?;
        match &self.outcome {
            TerminalOutcome::Success => writer.string_field("outcome", "success"),
            TerminalOutcome::Cancelled => writer.string_field("outcome", "cancelled"),
            TerminalOutcome::Error(error) => {
                writer.string_field("outcome", "error")?;
                writer.string_field("error", error.kind.code())?;
                writer.string_field("diagnostic", error.diagnostic.as_str())
            }
        }
    }
}

struct JsonInteger(u64);

impl JsonPayload for JsonInteger {
    fn encoded_len(&self) -> Result<usize, RpcError> {
        Ok(decimal_len(self.0))
    }

    fn write_json(&self, destination: &mut [u8]) -> Result<usize, RpcError> {
        let length = decimal_len(self.0);
        let capacity = destination.len();
        let output = destination
            .get_mut(..length)
            .ok_or(RpcError::FrameTooLarge {
                size: length,
                capacity,
            })?;
        let mut value = self.0;
        for index in (0..length).rev() {
            let digit = u8::try_from(value % 10).map_err(|_error| RpcError::InvalidFrameState)?;
            let encoded = b'0'.checked_add(digit).ok_or(RpcError::InvalidFrameState)?;
            *output.get_mut(index).ok_or(RpcError::InvalidFrameState)? = encoded;
            value /= 10;
        }
        Ok(length)
    }
}

const fn decimal_len(mut value: u64) -> usize {
    let mut length = 1_usize;
    while value >= 10 {
        value /= 10;
        length = length.saturating_add(1);
    }
    length
}

fn utf8_prefix(value: &str, max_bytes: usize) -> usize {
    let mut end = core::cmp::min(value.len(), max_bytes);
    while !value.is_char_boundary(end) {
        end = end.saturating_sub(1);
    }
    end
}

fn truncate_utf8(value: &str, max_bytes: usize) -> &str {
    value
        .get(..utf8_prefix(value, max_bytes))
        .unwrap_or_default()
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

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use super::{
        ExecutionError, ExecutionErrorKind, FinishedFields, JsonInteger, JsonObjectPayload,
        JsonPayload, OutputFields, TerminalOutcome, VM_DIAGNOSTIC_BYTES, VM_EVENT_INPUT_BYTES,
        VM_OUTPUT_CHUNK_BYTES, decimal_len, truncate_utf8, utf8_prefix,
    };

    #[test]
    fn integer_payload_writes_without_an_intermediate_string() {
        let mut output = [0_u8; 20];
        let written = JsonInteger(u64::MAX)
            .write_json(&mut output)
            .expect("write integer");
        assert_eq!(
            output.get(..written).expect("written integer range"),
            b"18446744073709551615"
        );
        assert_eq!(decimal_len(u64::MAX), 20);
        assert_eq!(decimal_len(0), 1);
    }

    #[test]
    fn utf8_chunks_and_diagnostics_end_at_character_boundaries() {
        assert_eq!(utf8_prefix("abc", 2), 2);
        assert_eq!(utf8_prefix("aé", 2), 1);
        assert_eq!(truncate_utf8("aé", 2), "a");
    }

    #[test]
    fn worst_case_event_inputs_fit_the_declared_bound() {
        let text = "\0".repeat(VM_OUTPUT_CHUNK_BYTES);
        let output = OutputFields {
            run_id: u32::MAX,
            sequence: u64::MAX,
            chunk: &text,
            message_end: false,
        };
        assert!(
            JsonObjectPayload::new(&output)
                .encoded_len()
                .expect("measure output Event")
                <= VM_EVENT_INPUT_BYTES
        );

        let diagnostic = "\0".repeat(VM_DIAGNOSTIC_BYTES);
        let finished = FinishedFields {
            run_id: u32::MAX,
            sequence: u64::MAX,
            outcome: TerminalOutcome::Error(ExecutionError::new(
                ExecutionErrorKind::UnexpectedYield,
                &diagnostic,
            )),
        };
        assert!(
            JsonObjectPayload::new(&finished)
                .encoded_len()
                .expect("measure finished Event")
                <= VM_EVENT_INPUT_BYTES
        );
    }
}
