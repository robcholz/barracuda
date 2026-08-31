use alloc::string::{String, ToString};
use core::{future::Future, pin::Pin, task::Poll};

use barracuda_event_router::{RpcFrame, RpcHandler, RpcMethod, RpcResult, RpcStream, Streaming};
use barracuda_lua::{Error as LuaError, ErrorKind as LuaErrorKind, LuaExecution};
use barracuda_runtime_utils::yield_stream::{Yielder, try_yield_stream};
use barracuda_vm_builtin_packages::{
    BuiltinPackages,
    io::{Input as LuaInput, Output as LuaOutput},
};
use barracuda_vm_package_api::LuaPackageRegistry;
use getset::CopyGetters;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use zerocopy::{Immutable, IntoBytes, KnownLayout, TryFromBytes};

use crate::VmLimits;
use crate::component::create_lua;

const TEXT_CAPACITY: usize = 62;
const TEXT_MAX_BYTES: usize = TEXT_CAPACITY - 1;
const ERROR_TEXT_CAPACITY: usize = 63;
const ERROR_TEXT_MAX_BYTES: usize = ERROR_TEXT_CAPACITY - 1;

/// Whether a logical source, input, or output message continues in another frame.
#[repr(u8)]
#[derive(
    Clone,
    Copy,
    Debug,
    Deserialize,
    Eq,
    Immutable,
    IntoBytes,
    KnownLayout,
    PartialEq,
    Serialize,
    TryFromBytes,
)]
pub enum ChunkBoundary {
    /// More frames belong to the current logical message.
    More = 0,
    /// This frame completes the current logical message.
    Complete = 1,
}

/// Logical request stream carried by a `vm.run` frame.
#[repr(u8)]
#[derive(
    Clone,
    Copy,
    Debug,
    Deserialize,
    Eq,
    Immutable,
    IntoBytes,
    KnownLayout,
    PartialEq,
    Serialize,
    TryFromBytes,
)]
pub enum RunRequestKind {
    /// Lua source bytes, sent before execution starts.
    Source = 0,
    /// One `io.input()` message, sent after source completion.
    Input = 1,
}

/// Failure to construct or decode a fixed-size NUL-terminated text field.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum FrameTextError {
    /// Text exceeds the field's UTF-8 byte capacity.
    #[error("text exceeds the RPC frame capacity")]
    TooLong,
    /// Text contains an interior NUL byte.
    #[error("text contains a NUL byte")]
    ContainsNul,
    /// Wire bytes are not a canonical NUL-terminated field.
    #[error("text is not canonically NUL terminated")]
    InvalidTerminator,
    /// Wire bytes before the terminator are not UTF-8.
    #[error("text is not valid UTF-8")]
    InvalidUtf8,
}

/// Fixed-capacity, NUL-terminated UTF-8 text carried in request/response frames.
#[repr(transparent)]
#[derive(Clone, Copy, Debug, Eq, Immutable, IntoBytes, KnownLayout, PartialEq, TryFromBytes)]
#[doc(hidden)]
pub struct VmTextChunk([u8; TEXT_CAPACITY]);

impl VmTextChunk {
    fn new(text: &str) -> Result<Self, FrameTextError> {
        Ok(Self(encode_text(text)?))
    }

    /// Decodes the canonical UTF-8 C string.
    fn as_str(&self) -> Result<&str, FrameTextError> {
        decode_text(&self.0)
    }
}

impl Serialize for VmTextChunk {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        self.as_str()
            .map_err(serde::ser::Error::custom)?
            .serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for VmTextChunk {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let text = String::deserialize(deserializer)?;
        Self::new(&text).map_err(serde::de::Error::custom)
    }
}

/// One fixed 64-byte request frame for [`Run`].
#[repr(C)]
#[derive(
    Clone,
    Copy,
    Debug,
    Deserialize,
    Eq,
    Immutable,
    IntoBytes,
    KnownLayout,
    PartialEq,
    Serialize,
    TryFromBytes,
    CopyGetters,
)]
pub struct RunRequestFrame {
    /// Logical request channel carried by this frame.
    #[getset(get_copy = "pub")]
    kind: RunRequestKind,
    /// Whether this frame completes the logical message.
    #[getset(get_copy = "pub")]
    boundary: ChunkBoundary,
    text: VmTextChunk,
}

impl RunRequestFrame {
    /// Creates one Lua source frame.
    pub fn source(text: &str, boundary: ChunkBoundary) -> Result<Self, FrameTextError> {
        Self::new(RunRequestKind::Source, text, boundary)
    }

    /// Creates one Lua `io.input()` frame.
    pub fn input(text: &str, boundary: ChunkBoundary) -> Result<Self, FrameTextError> {
        Self::new(RunRequestKind::Input, text, boundary)
    }

    fn new(
        kind: RunRequestKind,
        text: &str,
        boundary: ChunkBoundary,
    ) -> Result<Self, FrameTextError> {
        Ok(Self {
            kind,
            boundary,
            text: VmTextChunk::new(text)?,
        })
    }

    /// Decodes this frame's UTF-8 text chunk.
    pub fn text(&self) -> Result<&str, FrameTextError> {
        self.text.as_str()
    }
}

/// One fixed 64-byte response frame containing part of one `io.print(...)` message.
#[repr(C)]
#[derive(
    Clone,
    Copy,
    Debug,
    Deserialize,
    Eq,
    Immutable,
    IntoBytes,
    KnownLayout,
    PartialEq,
    Serialize,
    TryFromBytes,
    CopyGetters,
)]
pub struct RunResponseFrame {
    /// Whether this frame completes the current printed message.
    #[getset(get_copy = "pub")]
    boundary: ChunkBoundary,
    reserved: u8,
    text: VmTextChunk,
}

impl RunResponseFrame {
    fn new(text: &str, boundary: ChunkBoundary) -> Result<Self, FrameTextError> {
        Ok(Self {
            boundary,
            reserved: 0,
            text: VmTextChunk::new(text)?,
        })
    }

    /// Decodes this frame's UTF-8 text chunk.
    pub fn text(&self) -> Result<&str, FrameTextError> {
        self.text.as_str()
    }
}

/// Business-level terminal status returned by `vm.run`.
#[repr(u8)]
#[derive(
    Clone,
    Copy,
    Debug,
    Deserialize,
    Eq,
    Immutable,
    IntoBytes,
    KnownLayout,
    PartialEq,
    Serialize,
    TryFromBytes,
)]
pub enum RunErrorKind {
    /// Request frames violated the source-then-input state machine.
    InvalidProtocol = 0,
    /// A request carried invalid fixed-field text.
    InvalidText = 1,
    /// Complete source exceeded [`VmLimits::max_source_bytes`].
    SourceLimitExceeded = 2,
    /// One logical input message exceeded [`VmLimits::max_input_bytes`].
    InputLimitExceeded = 3,
    /// The host could not create a Lua state.
    VmCreate = 4,
    /// The host-created state or execution IO could not be configured.
    VmConfigure = 5,
    /// Lua rejected the source chunk.
    LuaLoad = 6,
    /// Lua execution failed.
    LuaRuntime = 7,
    /// Lua yielded outside the wrapper's async binding protocol.
    UnexpectedYield = 8,
    /// A printed message could not be represented by the output protocol.
    OutputEncoding = 9,
    /// The VM Plugin has not started its Embassy runtime.
    RuntimeUnavailable = 10,
    /// All VM Embassy task slots are occupied.
    Busy = 11,
    /// The fixed Lua heap assigned to this execution was exhausted.
    LuaMemory = 12,
}

/// Fixed-capacity NUL-terminated UTF-8 diagnostic attached to [`RunError`].
#[repr(transparent)]
#[derive(Clone, Copy, Debug, Eq, Immutable, IntoBytes, KnownLayout, PartialEq, TryFromBytes)]
#[doc(hidden)]
pub struct VmErrorText([u8; ERROR_TEXT_CAPACITY]);

impl VmErrorText {
    fn from_lossy(message: &str) -> Self {
        let prefix = message.split('\0').next().unwrap_or_default();
        let mut end = core::cmp::min(prefix.len(), ERROR_TEXT_MAX_BYTES);
        while !prefix.is_char_boundary(end) {
            end = end.saturating_sub(1);
        }
        let text = prefix.get(..end).unwrap_or_default();
        let mut bytes = [0_u8; ERROR_TEXT_CAPACITY];
        if let Some(target) = bytes.get_mut(..text.len()) {
            target.copy_from_slice(text.as_bytes());
        }
        Self(bytes)
    }

    /// Decodes the diagnostic string.
    fn as_str(&self) -> Result<&str, FrameTextError> {
        decode_text(&self.0)
    }
}

impl Serialize for VmErrorText {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        self.as_str()
            .map_err(serde::ser::Error::custom)?
            .serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for VmErrorText {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let text = String::deserialize(deserializer)?;
        if text.as_bytes().contains(&0) || text.len() > ERROR_TEXT_MAX_BYTES {
            return Err(serde::de::Error::custom("invalid VM error text"));
        }
        Ok(Self::from_lossy(&text))
    }
}

/// Fixed 64-byte terminal method error for [`Run`].
#[repr(C)]
#[derive(
    Clone,
    Copy,
    Debug,
    Deserialize,
    Eq,
    Immutable,
    IntoBytes,
    KnownLayout,
    PartialEq,
    Serialize,
    TryFromBytes,
    CopyGetters,
)]
pub struct RunError {
    /// Stable error category.
    #[getset(get_copy = "pub")]
    kind: RunErrorKind,
    diagnostic: VmErrorText,
}

impl RunError {
    pub(crate) fn new(kind: RunErrorKind, diagnostic: &str) -> Self {
        Self {
            kind,
            diagnostic: VmErrorText::from_lossy(diagnostic),
        }
    }

    /// Decodes the bounded UTF-8 diagnostic.
    pub fn diagnostic(&self) -> Result<&str, FrameTextError> {
        self.diagnostic.as_str()
    }
}

/// Runs one isolated Lua state over bidirectional request/response streams.
pub struct Run;

impl RpcMethod for Run {
    const ADDRESS: &'static str = "vm.run";
    type Request = RunRequestFrame;
    type Response = RunResponseFrame;
    type Error = RunError;
    type Input = Streaming;
    type Output = Streaming;
}

/// Builds the reusable `vm.run` handler.
pub fn run_handler(limits: VmLimits, builtin_packages: BuiltinPackages) -> impl RpcHandler<Run> {
    run_handler_with_registry(limits, builtin_packages, LuaPackageRegistry::new())
}

pub(crate) fn run_handler_with_registry(
    limits: VmLimits,
    builtin_packages: BuiltinPackages,
    package_registry: LuaPackageRegistry,
) -> impl RpcHandler<Run> {
    move |_context, requests: RpcStream<RpcFrame<RunRequestFrame>>| {
        let package_registry = package_registry.clone();
        async move {
            let stream = run_stream(
                requests,
                limits,
                builtin_packages,
                package_registry,
                None,
                None,
            );
            Ok(RpcStream::new(stream))
        }
    }
}

pub(crate) type RunItem = Result<RunResponseFrame, RunError>;

pub(crate) fn run_stream(
    requests: RpcStream<RpcFrame<RunRequestFrame>>,
    limits: VmLimits,
    builtin_packages: BuiltinPackages,
    package_registry: LuaPackageRegistry,
    yield_signal: Option<crate::runtime::VmYieldSignal>,
    memory: Option<crate::memory::VmMemoryLease>,
) -> impl futures_core::Stream<Item = RpcResult<RunItem>> + 'static {
    try_yield_stream(move |yielder| async move {
        drive_run(
            yielder,
            requests,
            limits,
            builtin_packages,
            package_registry,
            yield_signal,
            memory,
        )
        .await
    })
}

async fn drive_run(
    yielder: Yielder<RunItem>,
    mut requests: RpcStream<RpcFrame<RunRequestFrame>>,
    limits: VmLimits,
    builtin_packages: BuiltinPackages,
    package_registry: LuaPackageRegistry,
    yield_signal: Option<crate::runtime::VmYieldSignal>,
    memory: Option<crate::memory::VmMemoryLease>,
) -> RpcResult<()> {
    let source = match collect_source(&mut requests, limits).await? {
        Ok(source) => source,
        Err(error) => return emit_error(&yielder, error).await,
    };
    let lua = match memory.as_ref() {
        // `memory` is a function parameter, so it outlives the local Lua state
        // and the LuaExecution that consumes it below.
        Some(memory) => unsafe { memory.create_lua() },
        None => create_lua(),
    };
    let mut lua = match lua {
        Ok(lua) => lua,
        Err(error) => return emit_error(&yielder, factory_error(&error)).await,
    };
    let (input, mut output) = match builtin_packages.install(&mut lua) {
        Ok(installed) => installed.into_io(),
        Err(error) => {
            return emit_error(
                &yielder,
                RunError::new(RunErrorKind::VmConfigure, error.message()),
            )
            .await;
        }
    };
    if let Err(error) = package_registry.install(&mut lua) {
        return emit_error(
            &yielder,
            RunError::new(RunErrorKind::VmConfigure, error.message()),
        )
        .await;
    }
    if let Some(yield_signal) = yield_signal
        && let Err(error) =
            lua.set_instruction_hook(limits.instruction_hook_interval(), move || {
                yield_signal.mark();
            })
    {
        return emit_error(
            &yielder,
            RunError::new(RunErrorKind::VmConfigure, error.message()),
        )
        .await;
    }
    let mut execution = lua.run(&source);
    drive_started(
        &yielder,
        &mut requests,
        input,
        &mut output,
        &mut execution,
        limits,
    )
    .await
}

async fn collect_source(
    requests: &mut RpcStream<RpcFrame<RunRequestFrame>>,
    limits: VmLimits,
) -> RpcResult<Result<String, RunError>> {
    let mut source = String::new();
    loop {
        let Some(frame) = requests.next().await else {
            return Ok(Err(RunError::new(
                RunErrorKind::InvalidProtocol,
                "request ended before source completion",
            )));
        };
        let frame = *frame?.view()?;
        if frame.kind() != RunRequestKind::Source {
            return Ok(Err(RunError::new(
                RunErrorKind::InvalidProtocol,
                "input arrived before source completion",
            )));
        }
        let text = match frame.text() {
            Ok(text) => text,
            Err(error) => {
                return Ok(Err(RunError::new(
                    RunErrorKind::InvalidText,
                    &error.to_string(),
                )));
            }
        };
        if append_limited(&mut source, text, limits.max_source_bytes()).is_err() {
            return Ok(Err(RunError::new(
                RunErrorKind::SourceLimitExceeded,
                "Lua source exceeds the configured limit",
            )));
        }
        if frame.boundary() == ChunkBoundary::Complete {
            return Ok(Ok(source));
        }
    }
}

async fn drive_started(
    yielder: &Yielder<RunItem>,
    requests: &mut RpcStream<RpcFrame<RunRequestFrame>>,
    input: LuaInput,
    output: &mut LuaOutput,
    execution: &mut LuaExecution,
    limits: VmLimits,
) -> RpcResult<()> {
    let mut pending_input = String::new();
    let mut has_pending_input = false;
    loop {
        match next_event(requests, output, execution).await {
            RunEvent::Output(Some(message)) => {
                if let Err(error) = emit_output(yielder, &message).await {
                    return emit_error(yielder, error).await;
                }
            }
            RunEvent::Output(None) => {}
            RunEvent::Execution(result) => {
                input.close();
                return finish_execution(yielder, output, result).await;
            }
            RunEvent::Request(Some(frame)) => {
                let frame = *frame?.view()?;
                if frame.kind() != RunRequestKind::Input {
                    return emit_error(
                        yielder,
                        RunError::new(
                            RunErrorKind::InvalidProtocol,
                            "source frame arrived after execution started",
                        ),
                    )
                    .await;
                }
                let text = match frame.text() {
                    Ok(text) => text,
                    Err(error) => {
                        return emit_error(
                            yielder,
                            RunError::new(RunErrorKind::InvalidText, &error.to_string()),
                        )
                        .await;
                    }
                };
                if append_limited(&mut pending_input, text, limits.max_input_bytes()).is_err() {
                    return emit_error(
                        yielder,
                        RunError::new(
                            RunErrorKind::InputLimitExceeded,
                            "Lua input message exceeds the configured limit",
                        ),
                    )
                    .await;
                }
                has_pending_input = true;
                if frame.boundary() == ChunkBoundary::Complete {
                    let message = core::mem::take(&mut pending_input);
                    has_pending_input = false;
                    match send_input(yielder, &input, message, output, execution).await? {
                        SendOutcome::Sent | SendOutcome::Closed => {}
                        SendOutcome::Execution(result) => {
                            input.close();
                            return finish_execution(yielder, output, result).await;
                        }
                    }
                }
            }
            RunEvent::Request(None) => {
                if has_pending_input {
                    let message = core::mem::take(&mut pending_input);
                    if let SendOutcome::Execution(result) =
                        send_input(yielder, &input, message, output, execution).await?
                    {
                        input.close();
                        return finish_execution(yielder, output, result).await;
                    }
                }
                input.close();
                return drive_to_completion(yielder, output, execution).await;
            }
        }
    }
}

enum RunEvent {
    Request(Option<RpcResult<RpcFrame<RunRequestFrame>>>),
    Output(Option<String>),
    Execution(barracuda_lua::Result<()>),
}

async fn next_event(
    requests: &mut RpcStream<RpcFrame<RunRequestFrame>>,
    output: &mut LuaOutput,
    execution: &mut LuaExecution,
) -> RunEvent {
    let mut request = core::pin::pin!(requests.next());
    let mut next_output = core::pin::pin!(output.next());
    core::future::poll_fn(|context| {
        if let Poll::Ready(message) = next_output.as_mut().poll(context) {
            return Poll::Ready(RunEvent::Output(message));
        }
        if let Poll::Ready(result) = Pin::new(&mut *execution).poll(context) {
            return Poll::Ready(RunEvent::Execution(result));
        }
        request.as_mut().poll(context).map(RunEvent::Request)
    })
    .await
}

enum SendOutcome {
    Sent,
    Closed,
    Execution(barracuda_lua::Result<()>),
}

async fn send_input(
    yielder: &Yielder<RunItem>,
    input: &LuaInput,
    message: String,
    output: &mut LuaOutput,
    execution: &mut LuaExecution,
) -> RpcResult<SendOutcome> {
    let mut send = core::pin::pin!(input.send(message));
    loop {
        let mut next_output = core::pin::pin!(output.next());
        let event = core::future::poll_fn(|context| {
            if let Poll::Ready(message) = next_output.as_mut().poll(context) {
                return Poll::Ready(SendEvent::Output(message));
            }
            if let Poll::Ready(result) = Pin::new(&mut *execution).poll(context) {
                return Poll::Ready(SendEvent::Execution(result));
            }
            send.as_mut().poll(context).map(SendEvent::Send)
        })
        .await;
        match event {
            SendEvent::Output(Some(message)) => {
                if let Err(error) = emit_output(yielder, &message).await {
                    emit_error(yielder, error).await?;
                    return Ok(SendOutcome::Closed);
                }
            }
            SendEvent::Output(None) => {}
            SendEvent::Execution(result) => return Ok(SendOutcome::Execution(result)),
            SendEvent::Send(Ok(())) => return Ok(SendOutcome::Sent),
            SendEvent::Send(Err(_closed)) => return Ok(SendOutcome::Closed),
        }
    }
}

enum SendEvent {
    Send(barracuda_lua::Result<()>),
    Output(Option<String>),
    Execution(barracuda_lua::Result<()>),
}

async fn drive_to_completion(
    yielder: &Yielder<RunItem>,
    output: &mut LuaOutput,
    execution: &mut LuaExecution,
) -> RpcResult<()> {
    loop {
        let event = {
            let mut next_output = core::pin::pin!(output.next());
            core::future::poll_fn(|context| {
                if let Poll::Ready(message) = next_output.as_mut().poll(context) {
                    return Poll::Ready(CompletionEvent::Output(message));
                }
                Pin::new(&mut *execution)
                    .poll(context)
                    .map(CompletionEvent::Execution)
            })
            .await
        };
        match event {
            CompletionEvent::Output(Some(message)) => {
                if let Err(error) = emit_output(yielder, &message).await {
                    return emit_error(yielder, error).await;
                }
            }
            CompletionEvent::Output(None) => {}
            CompletionEvent::Execution(result) => {
                return finish_execution(yielder, output, result).await;
            }
        }
    }
}

enum CompletionEvent {
    Output(Option<String>),
    Execution(barracuda_lua::Result<()>),
}

async fn finish_execution(
    yielder: &Yielder<RunItem>,
    output: &mut LuaOutput,
    result: barracuda_lua::Result<()>,
) -> RpcResult<()> {
    while let Some(message) = output.next().await {
        if let Err(error) = emit_output(yielder, &message).await {
            return emit_error(yielder, error).await;
        }
    }
    if let Err(error) = result {
        emit_error(yielder, execution_error(&error)).await?;
    }
    Ok(())
}

async fn emit_output(yielder: &Yielder<RunItem>, message: &str) -> Result<(), RunError> {
    if message.as_bytes().contains(&0) {
        return Err(RunError::new(
            RunErrorKind::OutputEncoding,
            "printed text contains a NUL byte",
        ));
    }
    if message.is_empty() {
        let frame = RunResponseFrame::new("", ChunkBoundary::Complete)
            .map_err(|error| RunError::new(RunErrorKind::OutputEncoding, &error.to_string()))?;
        yielder.yield_one(Ok(frame)).await;
        return Ok(());
    }
    let mut remaining = message;
    while !remaining.is_empty() {
        let mut end = core::cmp::min(remaining.len(), TEXT_MAX_BYTES);
        while !remaining.is_char_boundary(end) {
            end = end.saturating_sub(1);
        }
        let chunk = remaining.get(..end).ok_or_else(|| {
            RunError::new(
                RunErrorKind::OutputEncoding,
                "could not split printed UTF-8",
            )
        })?;
        remaining = remaining.get(end..).ok_or_else(|| {
            RunError::new(
                RunErrorKind::OutputEncoding,
                "could not split printed UTF-8",
            )
        })?;
        let boundary = if remaining.is_empty() {
            ChunkBoundary::Complete
        } else {
            ChunkBoundary::More
        };
        let frame = RunResponseFrame::new(chunk, boundary)
            .map_err(|error| RunError::new(RunErrorKind::OutputEncoding, &error.to_string()))?;
        yielder.yield_one(Ok(frame)).await;
    }
    Ok(())
}

async fn emit_error(yielder: &Yielder<RunItem>, error: RunError) -> RpcResult<()> {
    yielder.yield_one(Err(error)).await;
    Ok(())
}

fn append_limited(target: &mut String, text: &str, limit: usize) -> Result<(), ()> {
    let length = target.len().checked_add(text.len()).ok_or(())?;
    if length > limit {
        return Err(());
    }
    target.push_str(text);
    Ok(())
}

fn encode_text<const N: usize>(text: &str) -> Result<[u8; N], FrameTextError> {
    if text.as_bytes().contains(&0) {
        return Err(FrameTextError::ContainsNul);
    }
    if text.len() >= N {
        return Err(FrameTextError::TooLong);
    }
    let mut bytes = [0_u8; N];
    bytes
        .get_mut(..text.len())
        .ok_or(FrameTextError::TooLong)?
        .copy_from_slice(text.as_bytes());
    Ok(bytes)
}

fn decode_text(bytes: &[u8]) -> Result<&str, FrameTextError> {
    let end = bytes
        .iter()
        .position(|byte| *byte == 0)
        .ok_or(FrameTextError::InvalidTerminator)?;
    if bytes
        .get(end..)
        .ok_or(FrameTextError::InvalidTerminator)?
        .iter()
        .any(|byte| *byte != 0)
    {
        return Err(FrameTextError::InvalidTerminator);
    }
    core::str::from_utf8(bytes.get(..end).ok_or(FrameTextError::InvalidTerminator)?)
        .map_err(|_error| FrameTextError::InvalidUtf8)
}

fn factory_error(error: &LuaError) -> RunError {
    let kind = match error.kind() {
        LuaErrorKind::Create => RunErrorKind::VmCreate,
        LuaErrorKind::Memory => RunErrorKind::LuaMemory,
        LuaErrorKind::Load
        | LuaErrorKind::Runtime
        | LuaErrorKind::Conversion
        | LuaErrorKind::UnexpectedYield => RunErrorKind::VmConfigure,
    };
    RunError::new(kind, error.message())
}

fn execution_error(error: &LuaError) -> RunError {
    let kind = match error.kind() {
        LuaErrorKind::Load => RunErrorKind::LuaLoad,
        LuaErrorKind::Memory => RunErrorKind::LuaMemory,
        LuaErrorKind::UnexpectedYield => RunErrorKind::UnexpectedYield,
        LuaErrorKind::Create | LuaErrorKind::Runtime | LuaErrorKind::Conversion => {
            RunErrorKind::LuaRuntime
        }
    };
    RunError::new(kind, error.message())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]
    #![allow(clippy::indexing_slicing)]

    use alloc::format;
    use alloc::string::ToString;
    use alloc::vec::Vec;

    use core::future::pending;
    use core::task::{Context, Waker};

    use futures_lite::future::block_on;

    use super::*;

    fn install_environment(lua: &mut barracuda_lua::Lua) -> (LuaInput, LuaOutput) {
        BuiltinPackages::all()
            .install(lua)
            .expect("install environment")
            .into_io()
    }

    #[test]
    fn response_chunking_preserves_utf8_and_message_boundary() {
        block_on(async {
            let message = "é".repeat(40);
            let mut stream = try_yield_stream(|yielder| async move {
                emit_output(&yielder, &message).await.map_err(|_| ())
            });
            let mut frames = Vec::new();
            while let Some(item) = futures_lite::StreamExt::next(&mut stream).await {
                frames.push(
                    item.expect("output stream")
                        .expect("response frame instead of method error"),
                );
            }
            assert_eq!(frames.len(), 2);
            assert_eq!(frames[0].boundary(), ChunkBoundary::More);
            assert_eq!(frames[1].boundary(), ChunkBoundary::Complete);
            assert_eq!(
                frames
                    .iter()
                    .map(|frame| frame.text().expect("valid text"))
                    .collect::<String>(),
                "é".repeat(40)
            );
        });
    }

    #[test]
    fn append_limit_is_checked_per_logical_value() {
        let mut value = "ab".to_string();
        assert_eq!(append_limited(&mut value, "c", 3), Ok(()));
        assert_eq!(append_limited(&mut value, "d", 3), Err(()));
        assert_eq!(value, "abc");
    }

    #[test]
    fn error_diagnostic_is_utf8_truncated_and_nul_safe() {
        let error = RunError::new(RunErrorKind::LuaRuntime, &("€".repeat(30) + "\0hidden"));
        let diagnostic = error.diagnostic().expect("valid diagnostic");
        assert!(diagnostic.len() <= ERROR_TEXT_MAX_BYTES);
        assert!(!diagnostic.contains('\0'));
        assert_eq!(error.kind(), RunErrorKind::LuaRuntime);
    }

    #[test]
    fn fixed_text_rejects_oversize_and_nul() {
        assert_eq!(
            VmTextChunk::new(&"x".repeat(TEXT_MAX_BYTES + 1)),
            Err(FrameTextError::TooLong)
        );
        assert_eq!(VmTextChunk::new("a\0b"), Err(FrameTextError::ContainsNul));
        assert_eq!(encode_text::<4>("abc"), Ok([b'a', b'b', b'c', 0]));
        assert_eq!(
            decode_text(&[b'a', 0, b'b']),
            Err(FrameTextError::InvalidTerminator)
        );
        assert_eq!(decode_text(&[0xff, 0]), Err(FrameTextError::InvalidUtf8));
        assert_eq!(decode_text(b"a"), Err(FrameTextError::InvalidTerminator));
        assert_eq!(TEXT_MAX_BYTES, 61);
    }

    #[test]
    fn output_rejects_an_interior_nul() {
        block_on(async {
            let mut stream = try_yield_stream(|yielder| async move {
                let error = emit_output(&yielder, "before\0after")
                    .await
                    .expect_err("NUL must be rejected");
                assert_eq!(error.kind(), RunErrorKind::OutputEncoding);
                Ok::<(), ()>(())
            });
            assert!(futures_lite::StreamExt::next(&mut stream).await.is_none());
        });
    }

    #[test]
    fn method_error_json_uses_a_string_diagnostic() {
        let error = RunError::new(RunErrorKind::LuaRuntime, "boom");
        let json = serde_json::to_string(&error).expect("serialize method error");
        assert!(json.contains("\"diagnostic\":\"boom\""));
        let decoded: RunError = serde_json::from_str(&json).expect("deserialize method error");
        assert_eq!(decoded, error);

        let oversized = format!(
            "{{\"kind\":\"LuaRuntime\",\"diagnostic\":\"{}\"}}",
            "x".repeat(ERROR_TEXT_CAPACITY)
        );
        assert!(serde_json::from_str::<RunError>(&oversized).is_err());
    }

    #[test]
    fn send_input_observes_execution_and_a_closed_input() {
        block_on(async {
            let mut lua = barracuda_lua::Lua::new().expect("create Lua");
            let (input, mut output) = install_environment(&mut lua);
            let mut execution = lua.run("return");
            let mut stream = try_yield_stream(|yielder| async move {
                let outcome = send_input(
                    &yielder,
                    &input,
                    String::from("unused"),
                    &mut output,
                    &mut execution,
                )
                .await
                .map_err(|_| ())?;
                assert!(matches!(outcome, SendOutcome::Execution(Ok(()))));
                Ok::<(), ()>(())
            });
            assert!(futures_lite::StreamExt::next(&mut stream).await.is_none());

            let mut lua = barracuda_lua::Lua::new().expect("create Lua");
            lua.register_async("wait_forever", |(): ()| async {
                pending::<()>().await;
                None::<barracuda_lua::Result<()>>
            })
            .expect("register wait");
            let (input, mut output) = install_environment(&mut lua);
            let mut execution = lua.run("wait_forever()");
            input.close();
            let mut stream = try_yield_stream(|yielder| async move {
                let outcome = send_input(
                    &yielder,
                    &input,
                    String::from("closed"),
                    &mut output,
                    &mut execution,
                )
                .await
                .map_err(|_| ())?;
                assert!(matches!(outcome, SendOutcome::Closed));
                Ok::<(), ()>(())
            });
            assert!(futures_lite::StreamExt::next(&mut stream).await.is_none());
        });
    }

    #[test]
    fn send_and_completion_forward_output_while_lua_is_suspended() {
        block_on(async {
            let mut lua = barracuda_lua::Lua::new().expect("create Lua");
            lua.register_async("wait_forever", |(): ()| async {
                pending::<()>().await;
                None::<barracuda_lua::Result<()>>
            })
            .expect("register wait");
            let (input, mut output) = install_environment(&mut lua);
            let mut execution =
                lua.run("local io = require('io'); io.print('queued'); wait_forever()");
            assert!(
                Pin::new(&mut execution)
                    .poll(&mut Context::from_waker(Waker::noop()))
                    .is_pending()
            );
            let mut stream = try_yield_stream(|yielder| async move {
                let outcome = send_input(
                    &yielder,
                    &input,
                    String::from("accepted"),
                    &mut output,
                    &mut execution,
                )
                .await
                .map_err(|_| ())?;
                assert!(matches!(outcome, SendOutcome::Sent));
                Ok::<(), ()>(())
            });
            let item = futures_lite::StreamExt::next(&mut stream)
                .await
                .expect("one output")
                .expect("stream success")
                .expect("response frame");
            assert_eq!(item.text(), Ok("queued"));
            assert!(futures_lite::StreamExt::next(&mut stream).await.is_none());

            let mut lua = barracuda_lua::Lua::new().expect("create Lua");
            lua.register_async("yield_once", |(): ()| async {
                let mut yielded = false;
                core::future::poll_fn(move |context| {
                    if yielded {
                        Poll::Ready(())
                    } else {
                        yielded = true;
                        context.waker().wake_by_ref();
                        Poll::Pending
                    }
                })
                .await;
                None::<barracuda_lua::Result<()>>
            })
            .expect("register yield");
            let (_input, mut output) = install_environment(&mut lua);
            let mut execution =
                lua.run("local io = require('io'); io.print('before completion'); yield_once()");
            assert!(
                Pin::new(&mut execution)
                    .poll(&mut Context::from_waker(Waker::noop()))
                    .is_pending()
            );
            let mut stream = try_yield_stream(|yielder| async move {
                drive_to_completion(&yielder, &mut output, &mut execution)
                    .await
                    .map_err(|_| ())
            });
            let item = futures_lite::StreamExt::next(&mut stream)
                .await
                .expect("one output")
                .expect("stream success")
                .expect("response frame");
            assert_eq!(item.text(), Ok("before completion"));
            assert!(futures_lite::StreamExt::next(&mut stream).await.is_none());
        });
    }
}
