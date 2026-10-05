//! Lua adapter for isolated, non-persistent Agent asks.

#![no_std]

extern crate alloc;

use alloc::{
    boxed::Box,
    rc::Rc,
    string::{String, ToString},
    sync::Arc,
    vec::Vec,
};
use core::{
    future::Future,
    pin::Pin,
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
};

use async_channel::{Receiver, Sender};
use barracuda_agent_plugin::{
    AgentRuntime, IterationEvent, Message, PermissionLevel, SessionEvent, SessionPersistence,
    TurnEvent, stream::StreamPart,
};
use barracuda_plugin::api::PluginContext;
use barracuda_plugin::manager::{
    Plugin, PluginError, PluginRegisterContext, PluginResult, PluginStartContext, PluginTaskToken,
};
use barracuda_vm_plugin::{
    Error, Lua, LuaPackage, LuaPackageRegistry, MetaMethod, Package, Result, UserData,
    UserDataHandle, UserDataMethods,
};
use embassy_futures::select::{Either, Either3, select, select3};
use futures_channel::oneshot;
use futures_util::stream::{FuturesUnordered, StreamExt as _};
use spin::Mutex;

/// Maximum UTF-8 byte length accepted by `agent.ask`.
pub const ASK_TEXT_MAX_BYTES: usize = 4 * 1024;
/// Maximum assistant-output bytes retained or emitted by one ask.
pub const ASK_OUTPUT_MAX_BYTES: usize = 32 * 1024;
/// Maximum number of Agent asks owned by this adapter at once.
pub const MAX_CONCURRENT_ASKS: usize = 2;

const REQUEST_QUEUE_DEPTH: usize = MAX_CONCURRENT_ASKS;
const OUTPUT_QUEUE_DEPTH: usize = 8;

/// Installs the require-only `agent` package into every VM Lua state.
#[barracuda_plugin::macros::plugin]
pub struct VmAgentPlugin {
    runtime: Option<VmAgentRuntime>,
}

struct VmAgentRuntime {
    agent: Rc<AgentRuntime>,
    requests: Receiver<AskJob>,
    state: Arc<PackageState>,
}

impl VmAgentPlugin {
    /// Creates an unregistered VM Agent adapter.
    #[must_use]
    pub fn new<Builtins, Io>(_context: &mut PluginContext<Builtins, Io>) -> Self {
        Self { runtime: None }
    }
}

impl Plugin for VmAgentPlugin {
    fn register<Storage>(
        &mut self,
        context: &mut PluginRegisterContext<'_, Storage>,
    ) -> PluginResult<()>
    where
        Storage: barracuda_plugin::manager::PluginStorage,
    {
        let agent = context.require::<AgentRuntime>("agent")?;
        let packages = context.require::<LuaPackageRegistry>("vm")?;
        let (package, requests) = AgentLuaPackage::new();
        let state = Arc::clone(&package.state);
        let registration = packages
            .register(package)
            .map_err(PluginError::registration)?;
        context.retain(registration);
        self.runtime = Some(VmAgentRuntime {
            agent,
            requests,
            state,
        });
        Ok(())
    }

    fn start<Storage>(&mut self, context: &mut PluginStartContext<'_, Storage>) -> PluginResult<()>
    where
        Storage: barracuda_plugin::manager::PluginStorage,
    {
        let runtime = self
            .runtime
            .take()
            .ok_or_else(|| PluginError::registration(VmAgentRuntimeUnavailable))?;
        let cancellation = context.task_token();
        let task = vm_agent_task(runtime, cancellation).map_err(PluginError::registration)?;
        context.task_spawner()?.spawn(task);
        Ok(())
    }
}

#[embassy_executor::task]
async fn vm_agent_task(runtime: VmAgentRuntime, cancellation: PluginTaskToken) {
    serve(runtime, cancellation).await;
}

type AskFuture = Pin<Box<dyn Future<Output = ()> + 'static>>;

async fn serve(runtime: VmAgentRuntime, cancellation: PluginTaskToken) {
    let VmAgentRuntime {
        agent,
        requests,
        state,
    } = runtime;
    let mut asks = FuturesUnordered::<AskFuture>::new();

    loop {
        if asks.is_empty() {
            match select(cancellation.cancelled(), requests.recv()).await {
                Either::First(()) | Either::Second(Err(_)) => break,
                Either::Second(Ok(request)) => {
                    asks.push(Box::pin(run_ask(Rc::clone(&agent), request)));
                }
            }
        } else {
            match select3(cancellation.cancelled(), requests.recv(), asks.next()).await {
                Either3::First(()) | Either3::Second(Err(_)) => break,
                Either3::Second(Ok(request)) => {
                    asks.push(Box::pin(run_ask(Rc::clone(&agent), request)));
                }
                Either3::Third(_) => {}
            }
        }
    }

    state.revoke();
    requests.close();
    while let Ok(request) = requests.try_recv() {
        let _ignored = request.accepted.send(Err(VmAgentError::RuntimeStopped));
    }
    while asks.next().await.is_some() {}
}

async fn run_ask(agent: Rc<AgentRuntime>, request: AskJob) {
    let AskJob {
        text,
        stream,
        accepted,
        output,
        cancel,
        _operation,
    } = request;

    if cancel.try_recv().is_ok() {
        let _ignored = accepted.send(Err(VmAgentError::Cancelled));
        return;
    }
    let session = match agent.new_session(SessionPersistence::Ephemeral).await {
        Ok(session) => session,
        Err(_) => {
            let _ignored = accepted.send(Err(VmAgentError::RuntimeStopped));
            return;
        }
    };
    if cancel.try_recv().is_ok() {
        let _ignored = accepted.send(Err(VmAgentError::Cancelled));
        let _ignored = agent.delete_session(session).await;
        return;
    }
    let (control, mut events) = match agent.open_session(session).await {
        Ok(opened) => opened,
        Err(_) => {
            let _ignored = accepted.send(Err(VmAgentError::RuntimeStopped));
            let _ignored = agent.delete_session(session).await;
            return;
        }
    };
    if control
        .set_permission_level(PermissionLevel::Deny)
        .await
        .is_err()
        || control.append(Message::text(text)).await.is_err()
    {
        let _ignored = accepted.send(Err(VmAgentError::RuntimeStopped));
        let _ignored = agent.delete_session(session).await;
        return;
    }
    if accepted.send(Ok(())).is_err() {
        let _ignored = agent.delete_session(session).await;
        return;
    }

    let mut mode = OutputMode::new(stream);
    let terminal = loop {
        match select(cancel.recv(), events.next()).await {
            Either::First(_) => break Some(VmAgentError::Cancelled),
            Either::Second(Some(Ok(SessionEvent::Turn(TurnEvent::Iteration(
                IterationEvent::Output(StreamPart::Delta(text)),
            )))))
            | Either::Second(Some(Ok(SessionEvent::Turn(TurnEvent::EffectOutput(
                StreamPart::Delta(text),
            ))))) => match mode.push(text) {
                Ok(Some(chunk)) => {
                    if !send_output(&output, &cancel, Ok(chunk)).await {
                        break Some(VmAgentError::Cancelled);
                    }
                }
                Ok(None) => {}
                Err(error) => break Some(error),
            },
            Either::Second(Some(Ok(SessionEvent::Turn(TurnEvent::Ended { .. })))) => break None,
            Either::Second(Some(Ok(SessionEvent::Turn(TurnEvent::InputRequested { .. })))) => {
                break Some(VmAgentError::InputRequired);
            }
            Either::Second(Some(Ok(SessionEvent::Turn(TurnEvent::Error(_)))))
            | Either::Second(Some(Ok(SessionEvent::Error(_)))) => {
                break Some(VmAgentError::TurnFailed);
            }
            Either::Second(Some(Ok(SessionEvent::Closed(_))))
            | Either::Second(Some(Err(_)))
            | Either::Second(None) => break Some(VmAgentError::RuntimeStopped),
            Either::Second(Some(Ok(_))) => {}
        }
    };

    let _ignored = agent.delete_session(session).await;
    match terminal {
        Some(VmAgentError::Cancelled) => {}
        Some(error) => {
            let _ignored = send_output(&output, &cancel, Err(error)).await;
        }
        None => {
            if let Some(answer) = mode.finish() {
                let _ignored = output.send(Ok(answer)).await;
            }
        }
    }
}

async fn send_output(
    output: &Sender<core::result::Result<String, VmAgentError>>,
    cancel: &Receiver<()>,
    item: core::result::Result<String, VmAgentError>,
) -> bool {
    match select(cancel.recv(), output.send(item)).await {
        Either::First(_) | Either::Second(Err(_)) => false,
        Either::Second(Ok(())) => true,
    }
}

struct OutputMode {
    stream: bool,
    bytes: usize,
    buffered: String,
}

impl OutputMode {
    fn new(stream: bool) -> Self {
        Self {
            stream,
            bytes: 0,
            buffered: String::new(),
        }
    }

    fn push(&mut self, text: String) -> core::result::Result<Option<String>, VmAgentError> {
        self.bytes = self
            .bytes
            .checked_add(text.len())
            .filter(|bytes| *bytes <= ASK_OUTPUT_MAX_BYTES)
            .ok_or(VmAgentError::OutputTooLarge)?;
        if self.stream {
            Ok(Some(text))
        } else {
            self.buffered.push_str(&text);
            Ok(None)
        }
    }

    fn finish(self) -> Option<String> {
        (!self.stream).then_some(self.buffered)
    }
}

struct AskJob {
    text: String,
    stream: bool,
    accepted: oneshot::Sender<core::result::Result<(), VmAgentError>>,
    output: Sender<core::result::Result<String, VmAgentError>>,
    cancel: Receiver<()>,
    _operation: OperationGuard,
}

struct AgentLuaPackage {
    state: Arc<PackageState>,
}

impl AgentLuaPackage {
    fn new() -> (Self, Receiver<AskJob>) {
        let (requests, receiver) = async_channel::bounded(REQUEST_QUEUE_DEPTH);
        let state = Arc::new(PackageState::new(requests));
        (
            Self {
                state: Arc::clone(&state),
            },
            receiver,
        )
    }
}

impl Package for AgentLuaPackage {
    fn install(&self, lua: &mut Lua) -> Result<()> {
        let state = Arc::clone(&self.state);
        lua.register_lib("agent", move |package| {
            package.register_async_with("ask", move |lua, (text, stream): (String, bool)| {
                let state = Arc::clone(&state);
                let prepared = prepare_ask(lua, state, text, stream);
                async move {
                    Some(match prepared {
                        Err(error) => Err(error),
                        Ok((output, accepted)) => match accepted.await {
                            Ok(Ok(())) => Ok(output),
                            Ok(Err(error)) => Err(Error::runtime(error.to_string())),
                            Err(_) => Err(Error::runtime(VmAgentError::RuntimeStopped.to_string())),
                        },
                    })
                }
            })
        })
    }
}

impl LuaPackage for AgentLuaPackage {
    fn name(&self) -> &'static str {
        "agent"
    }

    fn revoke(&self) {
        self.state.revoke();
    }
}

type AskAcceptance = oneshot::Receiver<core::result::Result<(), VmAgentError>>;
type PreparedAsk = (UserDataHandle<AgentOutput>, AskAcceptance);

fn prepare_ask(
    lua: &mut barracuda_vm_plugin::Context<'_>,
    state: Arc<PackageState>,
    text: String,
    stream: bool,
) -> Result<PreparedAsk> {
    if text.trim().is_empty() {
        return Err(Error::runtime(VmAgentError::InvalidRequest.to_string()));
    }
    if text.len() > ASK_TEXT_MAX_BYTES {
        return Err(Error::runtime(VmAgentError::InputTooLarge.to_string()));
    }
    let (operation, cancel) = state
        .begin()
        .map_err(|error| Error::runtime(error.to_string()))?;
    let cancel_sender = operation.cancel.clone();
    let (output_sender, output_receiver) = async_channel::bounded(OUTPUT_QUEUE_DEPTH);
    let (accepted_sender, accepted_receiver) = oneshot::channel();
    let output = lua.create_userdata(AgentOutput {
        output: output_receiver,
        cancel: cancel_sender,
        closed: AtomicBool::new(false),
    })?;
    let request = AskJob {
        text,
        stream,
        accepted: accepted_sender,
        output: output_sender,
        cancel,
        _operation: operation,
    };
    state.requests.try_send(request).map_err(|error| {
        if error.is_closed() {
            Error::runtime(VmAgentError::RuntimeStopped.to_string())
        } else {
            Error::runtime(VmAgentError::Busy.to_string())
        }
    })?;
    Ok((output, accepted_receiver))
}

struct AgentOutput {
    output: Receiver<core::result::Result<String, VmAgentError>>,
    cancel: Sender<()>,
    closed: AtomicBool,
}

impl AgentOutput {
    fn close(&self) {
        if !self.closed.swap(true, Ordering::AcqRel) {
            let _ignored = self.cancel.try_send(());
        }
    }
}

impl Drop for AgentOutput {
    fn drop(&mut self) {
        self.close();
    }
}

impl UserData for AgentOutput {
    fn add_methods(methods: &mut UserDataMethods<'_, Self>) {
        methods.add_async_method("next", |output, (): ()| async move {
            let receiver = output.with(|output| output.output.clone());
            Some(match receiver {
                Err(error) => Err(error),
                Ok(receiver) => match receiver.recv().await {
                    Ok(Ok(text)) => Ok(Some(text)),
                    Ok(Err(error)) => Err(Error::runtime(error.to_string())),
                    Err(_) => Ok(None),
                },
            })
        });
        methods.add_method("close", |output, (): ()| {
            output.close();
            None::<Result<()>>
        });
        methods.add_meta_method(MetaMethod::Close, |output, _error: Option<String>| {
            output.close();
            None::<Result<()>>
        });
    }
}

struct PackageState {
    requests: Sender<AskJob>,
    lifecycle: Mutex<Vec<(usize, Sender<()>)>>,
    next_operation: AtomicUsize,
    active_asks: AtomicUsize,
    active: AtomicBool,
}

impl PackageState {
    fn new(requests: Sender<AskJob>) -> Self {
        Self {
            requests,
            lifecycle: Mutex::new(Vec::new()),
            next_operation: AtomicUsize::new(0),
            active_asks: AtomicUsize::new(0),
            active: AtomicBool::new(true),
        }
    }

    fn begin(
        self: &Arc<Self>,
    ) -> core::result::Result<(OperationGuard, Receiver<()>), VmAgentError> {
        if !self.active.load(Ordering::Acquire) {
            return Err(VmAgentError::RuntimeStopped);
        }
        self.active_asks
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |active| {
                (active < MAX_CONCURRENT_ASKS).then_some(active + 1)
            })
            .map_err(|_| VmAgentError::Busy)?;

        let id = self.next_operation.fetch_add(1, Ordering::Relaxed);
        let (cancel, receiver) = async_channel::bounded(1);
        {
            let mut lifecycle = self.lifecycle.lock();
            if !self.active.load(Ordering::Acquire) {
                self.active_asks.fetch_sub(1, Ordering::AcqRel);
                return Err(VmAgentError::RuntimeStopped);
            }
            lifecycle.push((id, cancel.clone()));
        }
        Ok((
            OperationGuard {
                state: Arc::clone(self),
                id,
                cancel,
            },
            receiver,
        ))
    }

    fn revoke(&self) {
        self.active.store(false, Ordering::Release);
        self.requests.close();
        for (_, cancel) in self.lifecycle.lock().drain(..) {
            let _ignored = cancel.try_send(());
        }
    }
}

struct OperationGuard {
    state: Arc<PackageState>,
    id: usize,
    cancel: Sender<()>,
}

impl Drop for OperationGuard {
    fn drop(&mut self) {
        self.state.lifecycle.lock().retain(|(id, _)| *id != self.id);
        self.state.active_asks.fetch_sub(1, Ordering::AcqRel);
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
enum VmAgentError {
    #[error("Agent ask text must not be empty")]
    InvalidRequest,
    #[error("Agent ask text exceeds {ASK_TEXT_MAX_BYTES} bytes")]
    InputTooLarge,
    #[error("Agent ask capacity is exhausted")]
    Busy,
    #[error("Agent runtime is not available")]
    RuntimeStopped,
    #[error("Agent ask requires caller input")]
    InputRequired,
    #[error("Agent ask failed")]
    TurnFailed,
    #[error("Agent ask output exceeds {ASK_OUTPUT_MAX_BYTES} bytes")]
    OutputTooLarge,
    #[error("Agent ask was cancelled")]
    Cancelled,
}

#[derive(Debug, thiserror::Error)]
#[error("VM Agent runtime was not prepared during Plugin registration")]
struct VmAgentRuntimeUnavailable;

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    extern crate std;

    use alloc::string::String;
    use barracuda_vm_plugin::{Lua, Package};
    use futures_lite::future::{block_on, zip};

    use super::*;

    #[test]
    fn declaration_and_package_match_the_vm_agent_boundary() {
        use barracuda_plugin::manager::PluginDeclaration;

        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<AgentLuaPackage>();
        assert_eq!(VmAgentPlugin::ID, "vm-agent");
        assert_eq!(VmAgentPlugin::DEPENDS_ON, &["agent", "vm"]);
    }

    #[test]
    fn output_mode_streams_deltas_or_buffers_one_value() {
        let mut streaming = OutputMode::new(true);
        assert_eq!(
            streaming.push("Hel".into()).expect("accept delta"),
            Some("Hel".into())
        );
        assert_eq!(
            streaming.push("lo".into()).expect("accept delta"),
            Some("lo".into())
        );
        assert_eq!(streaming.finish(), None);

        let mut buffered = OutputMode::new(false);
        assert_eq!(buffered.push("Hel".into()).expect("buffer delta"), None);
        assert_eq!(buffered.push("lo".into()).expect("buffer delta"), None);
        assert_eq!(buffered.finish(), Some("Hello".into()));
    }

    #[test]
    fn request_and_output_limits_are_enforced() {
        let (package, _requests) = AgentLuaPackage::new();
        let mut lua = Lua::new().expect("create Lua");
        package.install(&mut lua).expect("install Agent package");
        let empty_rejected: bool = block_on(
            lua.load(
                "local agent=require('agent'); local output,err=agent.ask('   ',true); \
                 return output==nil and type(err)=='string'",
            )
            .eval_async(),
        )
        .expect("reject empty ask");
        assert!(empty_rejected);

        let mut mode = OutputMode::new(false);
        assert!(mode.push("x".repeat(ASK_OUTPUT_MAX_BYTES)).is_ok());
        assert_eq!(mode.push("x".into()), Err(VmAgentError::OutputTooLarge));
    }

    #[test]
    fn concurrent_ask_limit_releases_capacity_on_drop() {
        let (package, _requests) = AgentLuaPackage::new();
        let (first, _) = package.state.begin().expect("first ask");
        let (_second, _) = package.state.begin().expect("second ask");
        assert!(matches!(package.state.begin(), Err(VmAgentError::Busy)));
        drop(first);
        assert!(package.state.begin().is_ok());
    }

    #[test]
    fn lua_ask_returns_the_same_next_until_eof_interface_in_both_modes() {
        for (stream, sent, expected) in [
            (true, ["Hel", "lo"], ["Hel", "lo"]),
            (false, ["Hello", ""], ["Hello", ""]),
        ] {
            let (package, requests) = AgentLuaPackage::new();
            let mut lua = Lua::new().expect("create Lua");
            package.install(&mut lua).expect("install Agent package");
            let source = alloc::format!(
                "local agent=require('agent'); local output,err=agent.ask('hello',{}); \
                 assert(err==nil); local first,e1=output:next(); assert(e1==nil); \
                 local second,e2=output:next(); assert(e2==nil); \
                 local eof,e3=output:next(); return first, second or '', eof==nil and e3==nil",
                if stream { "true" } else { "false" },
            );
            let script = lua.load(&source).eval_async::<(String, String, bool)>();
            let provider = async {
                let request = requests.recv().await.expect("receive ask request");
                assert_eq!(request.text, "hello");
                assert_eq!(request.stream, stream);
                request.accepted.send(Ok(())).expect("accept request");
                for chunk in sent {
                    if !chunk.is_empty() {
                        request
                            .output
                            .send(Ok(chunk.into()))
                            .await
                            .expect("send output");
                    }
                }
            };
            let (result, ()) = block_on(zip(script, provider));
            let result = result.expect("run Lua");
            assert_eq!(result.0, expected[0]);
            assert_eq!(result.1, expected[1]);
            assert!(result.2);
        }
    }

    #[test]
    fn lua_ask_start_failure_uses_nil_error() {
        let (package, requests) = AgentLuaPackage::new();
        let mut lua = Lua::new().expect("create Lua");
        package.install(&mut lua).expect("install Agent package");
        let script = lua
            .load(
                "local agent=require('agent'); local output,err=agent.ask('hello',true); \
                 return output==nil, err",
            )
            .eval_async::<(bool, String)>();
        let provider = async {
            let request = requests.recv().await.expect("receive ask request");
            request
                .accepted
                .send(Err(VmAgentError::Busy))
                .expect("reject request");
        };
        let (result, ()) = block_on(zip(script, provider));
        let (missing, error) = result.expect("run Lua");
        assert!(missing);
        assert_eq!(error, "Agent ask capacity is exhausted");
    }

    #[test]
    fn revoked_package_rejects_new_asks() {
        let (package, _requests) = AgentLuaPackage::new();
        let mut lua = Lua::new().expect("create Lua");
        package.install(&mut lua).expect("install Agent package");
        package.revoke();

        let (missing, error): (bool, String) = block_on(
            lua.load(
                "local agent=require('agent'); local output,err=agent.ask('hello',true); \
                 return output==nil, err",
            )
            .eval_async(),
        )
        .expect("run Lua");
        assert!(missing);
        assert_eq!(error, "Agent runtime is not available");
    }
}
