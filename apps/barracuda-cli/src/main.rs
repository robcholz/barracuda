//! `barracuda` — a minimal REPL that drives the whole agent runtime through
//! the public [`barracuda_agent_runtime`] API: build an [`AgentRuntime`], create a session,
//! append user text, and print each turn's replies.
//!
//! LLM config is read from this crate's `.env.local`:
//! `BARRACUDA_LLM_API_KEY`, `BARRACUDA_LLM_BASE_URL`, and `BARRACUDA_LLM_MODEL`. Memory is
//! written under this crate's `output/barracuda/`.
//!
//! ```
//! cargo run -p barracuda-cli --bin barracuda
//! ```
//!
mod command;
mod line_editor;

use std::collections::VecDeque;
use std::convert::Infallible;
use std::ffi::CStr;
use std::fs::File;
use std::future::Future;
use std::io::{self, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use anstyle::{AnsiColor, Style};
use anyhow::{anyhow, bail, Result};
use barracuda_agent_runtime::{
    stream::StreamPart, AgentRuntime, ApiPurpose, BackendKind, InputRequestId, InputRequestKind,
    IterationEvent, Message, ModelApiConfig, ModelApiFactory, ProviderUsage, RuntimeStorageConfig,
    SessionControl, SessionError, SessionEvent, SessionId, SessionPersistence, SessionStream,
    ToolCall, ToolOutput, TurnEvent, TurnOrigin,
};
use barracuda_agent_trace::{FlatTreeSubscriber, LevelFilter, LogOutput, TraceSink};
use barracuda_fs::DiskFs;
use barracuda_model_api::{Certificate, ModelApi, Tls, TlsConfig, TlsVersion, X509};
use barracuda_net::TokioStack;
use embassy_time::{Duration, Ticker};
use futures_lite::StreamExt;
use rand_core::{TryCryptoRng, TryRng};
use static_cell::StaticCell;

use command::{
    parse_input, CliInput, PermissionLevelArg, ReasoningEffortArg, SessionPersistenceArg,
};
use line_editor::{ChatLineEditor, LineInput};

const MEMORY_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/output/barracuda");
const WAITING_TICK: Duration = Duration::from_millis(400);
const SYSTEM_CA_BUNDLE_CANDIDATES: &[&str] = &[
    "/etc/ssl/cert.pem",
    "/etc/ssl/certs/ca-certificates.crt",
    "/etc/pki/tls/certs/ca-bundle.crt",
    "/etc/pki/ca-trust/extracted/pem/tls-ca-bundle.pem",
    "/etc/ssl/ca-bundle.pem",
    "/usr/local/share/certs/ca-root-nss.crt",
];

type ChatRuntime = AgentRuntime<DiskFs, TokioStack>;

static NETWORK: TokioStack = TokioStack;
static TLS_RNG: StaticCell<SystemRng> = StaticCell::new();
static TLS_RUNTIME: StaticCell<Tls<'static>> = StaticCell::new();

struct SystemRng(File);

impl SystemRng {
    fn open() -> Result<Self> {
        Ok(Self(File::open("/dev/urandom")?))
    }

    fn fill_or_abort(&mut self, bytes: &mut [u8]) {
        if std::io::Read::read_exact(&mut self.0, bytes).is_err() {
            std::process::abort();
        }
    }
}

impl TryRng for SystemRng {
    type Error = Infallible;

    fn try_next_u32(&mut self) -> Result<u32, Self::Error> {
        let mut bytes = [0; 4];
        self.fill_or_abort(&mut bytes);
        Ok(u32::from_ne_bytes(bytes))
    }

    fn try_next_u64(&mut self) -> Result<u64, Self::Error> {
        let mut bytes = [0; 8];
        self.fill_or_abort(&mut bytes);
        Ok(u64::from_ne_bytes(bytes))
    }

    fn try_fill_bytes(&mut self, bytes: &mut [u8]) -> Result<(), Self::Error> {
        self.fill_or_abort(bytes);
        Ok(())
    }
}

impl TryCryptoRng for SystemRng {}

struct ChatTraceSink {
    file: Mutex<File>,
}

impl TraceSink for ChatTraceSink {
    fn write_line(&self, _level: tracing::Level, _tag: &str, line: &str) {
        let mut file = self
            .file
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let _ = writeln!(file, "{line}");
    }
}

struct ChatDriver {
    session: SessionId,
    control: SessionControl,
    events: SessionStream,
    content: ContentRenderer,
    active_origin: Option<TurnOrigin>,
    saw_output: bool,
}

impl ChatDriver {
    fn new(session: SessionId, control: SessionControl, events: SessionStream) -> Self {
        Self {
            session,
            control,
            events,
            content: ContentRenderer::default(),
            active_origin: None,
            saw_output: false,
        }
    }

    async fn append(&self, text: impl Into<String>) -> bool {
        if let Err(error) = self.control.append(Message::text(text)).await {
            print_event("error", &error.to_string(), EventStyle::Error);
            return false;
        }
        true
    }

    async fn respond(&self, request: InputRequestId, text: impl Into<String>) -> bool {
        if let Err(error) = self
            .control
            .respond(request, Message::text(text.into()))
            .await
        {
            print_event("error", &error.to_string(), EventStyle::Error);
            return false;
        }
        true
    }

    async fn set_permission_level(&self, level: PermissionLevelArg) -> bool {
        if let Err(error) = self.control.set_permission_level(level.into()).await {
            print_event("error", &error.to_string(), EventStyle::Error);
            return false;
        }
        let level_name: &'static str = level.into();
        print_event(
            "permission",
            &format!("set to {level_name}"),
            EventStyle::Permission,
        );
        true
    }

    async fn set_reasoning_effort(&self, effort: ReasoningEffortArg) -> bool {
        if let Err(error) = self.control.set_reasoning_effort(effort.into()).await {
            print_event("error", &error.to_string(), EventStyle::Error);
            return false;
        }
        let effort_name: &'static str = effort.into();
        print_event(
            "reasoning effort",
            &format!("set to {effort_name}"),
            EventStyle::Permission,
        );
        true
    }

    async fn interrupt(&self) -> bool {
        if let Err(error) = self.control.interrupt().await {
            print_event("error", &error.to_string(), EventStyle::Error);
            return false;
        }
        true
    }

    async fn cancel(&self) -> bool {
        if let Err(error) = self.control.cancel().await {
            print_event("error", &error.to_string(), EventStyle::Error);
            return false;
        }
        true
    }

    fn render(
        &mut self,
        event: SessionEvent,
        editor: &mut ChatLineEditor,
        above_prompt: bool,
        total_usage: &mut ProviderUsage,
    ) -> Result<RenderOutcome> {
        let outcome = match event {
            SessionEvent::Turn(TurnEvent::Started { origin, .. }) => {
                self.content.start_turn(above_prompt, editor)?;
                self.active_origin = Some(origin);
                self.saw_output = false;
                RenderOutcome::TurnStarted
            }
            SessionEvent::Turn(TurnEvent::InputRequested { request, kind }) => {
                self.content
                    .output(StreamPart::Delta(format_input_request(&kind)), editor)?;
                self.content.output(StreamPart::End, editor)?;
                self.content.finish(editor)?;
                RenderOutcome::InputRequested(request)
            }
            SessionEvent::Turn(TurnEvent::Iteration(IterationEvent::Reasoning(part))) => {
                self.content.reasoning(part, editor)?;
                RenderOutcome::Continue
            }
            SessionEvent::Turn(TurnEvent::EffectOutput(part))
            | SessionEvent::Turn(TurnEvent::Iteration(IterationEvent::Output(part))) => {
                self.saw_output |= self.content.output(part, editor)?;
                RenderOutcome::Continue
            }
            SessionEvent::Turn(TurnEvent::Iteration(IterationEvent::ToolResult(part))) => {
                self.content.tool_result(part, editor)?;
                RenderOutcome::Continue
            }
            SessionEvent::Turn(TurnEvent::Iteration(IterationEvent::Usage { usage })) => {
                accumulate_usage(total_usage, usage);
                self.content.finish(editor)?;
                self.content
                    .event(editor, "usage", &format_usage(usage), EventStyle::Usage)?;
                RenderOutcome::Continue
            }
            SessionEvent::Error(error) => {
                self.content.finish(editor)?;
                self.content
                    .event(editor, "error", &error.to_string(), EventStyle::Error)?;
                RenderOutcome::Continue
            }
            SessionEvent::Turn(TurnEvent::Error(error)) => {
                self.content.finish(editor)?;
                self.content
                    .event(editor, "error", &error.to_string(), EventStyle::Error)?;
                RenderOutcome::Continue
            }
            SessionEvent::Turn(TurnEvent::Ended { .. }) => {
                self.content.finish(editor)?;
                let user = matches!(self.active_origin.take(), Some(TurnOrigin::User));
                let saw_output = std::mem::take(&mut self.saw_output);
                RenderOutcome::TurnEnded { user, saw_output }
            }
            SessionEvent::Closed(_) => {
                self.content.finish(editor)?;
                RenderOutcome::Closed
            }
            SessionEvent::Turn(TurnEvent::Iteration(
                IterationEvent::Started { .. } | IterationEvent::Ended,
            )) => RenderOutcome::Continue,
        };
        Ok(outcome)
    }
}

#[derive(Default)]
struct PendingSessionSettings {
    permission: Option<PermissionLevelArg>,
    reasoning_effort: Option<ReasoningEffortArg>,
}

impl PendingSessionSettings {
    fn set_permission(&mut self, level: PermissionLevelArg) {
        self.permission = Some(level);
        let level_name: &'static str = level.into();
        print_event(
            "permission",
            &format!("will use {level_name} when a session starts"),
            EventStyle::Permission,
        );
    }

    fn set_reasoning_effort(&mut self, effort: ReasoningEffortArg) {
        self.reasoning_effort = Some(effort);
        let effort_name: &'static str = effort.into();
        print_event(
            "reasoning effort",
            &format!("will use {effort_name} when a session starts"),
            EventStyle::Permission,
        );
    }

    async fn apply(&mut self, chat: &ChatDriver) -> bool {
        if let Some(level) = self.permission {
            if !chat.set_permission_level(level).await {
                return false;
            }
            self.permission = None;
        }
        if let Some(effort) = self.reasoning_effort {
            if !chat.set_reasoning_effort(effort).await {
                return false;
            }
            self.reasoning_effort = None;
        }
        true
    }
}

async fn open_chat(runtime: &ChatRuntime, session: SessionId) -> Result<ChatDriver> {
    let (control, events) = runtime.open_session(session).await?;
    Ok(ChatDriver::new(session, control, events))
}

async fn new_persistent_chat(runtime: &ChatRuntime) -> Result<ChatDriver> {
    let session = runtime.new_session(SessionPersistence::Persistent).await?;
    match open_chat(runtime, session).await {
        Ok(chat) => Ok(chat),
        Err(error) => {
            runtime.delete_session(session).await.map_err(|rollback| {
                anyhow!(
                    "failed to open new session {session}: {error}; failed to remove it: {rollback}"
                )
            })?;
            Err(error)
        }
    }
}

async fn ensure_lazy_session<'a>(
    runtime: &ChatRuntime,
    chat: &'a mut Option<ChatDriver>,
) -> Result<&'a mut ChatDriver> {
    if chat.is_none() {
        let next = new_persistent_chat(runtime).await?;
        let session = next.session;
        print_event(
            "session",
            &format!("created persistent and entered {session}"),
            EventStyle::Control,
        );
        return Ok(chat.insert(next));
    }

    chat.as_mut()
        .ok_or_else(|| anyhow!("failed to access the active session"))
}

async fn switch_session(
    runtime: &ChatRuntime,
    chat: &mut Option<ChatDriver>,
    session: SessionId,
    verb: &str,
) -> Result<bool> {
    if chat.as_ref().is_some_and(|chat| chat.session == session) {
        print_event(
            "session",
            &format!("already active: {session}"),
            EventStyle::Control,
        );
        return Ok(false);
    }

    let next = open_chat(runtime, session).await?;
    let previous = chat.as_ref().map(|chat| chat.session);
    if let Some(current) = chat.as_ref() {
        if let Err(error) = current.control.close().await {
            drop(next);
            return Err(error.into());
        }
    }
    *chat = Some(next);
    let message = previous.map_or_else(
        || format!("{verb} and entered {session}"),
        |previous| format!("closed {previous}; {verb} and entered {session}"),
    );
    print_event("session", &message, EventStyle::Control);
    Ok(true)
}

async fn new_session(
    runtime: &ChatRuntime,
    chat: &mut Option<ChatDriver>,
    persistence: SessionPersistenceArg,
) -> Result<bool> {
    let persistence_name: &'static str = persistence.into();
    let session = runtime.new_session(persistence.into()).await?;
    let verb = format!("created {persistence_name}");
    match switch_session(runtime, chat, session, &verb).await {
        Ok(switched) => Ok(switched),
        Err(error) => {
            runtime.delete_session(session).await.map_err(|rollback| {
                anyhow!(
                    "failed to switch to new session {session}: {error}; failed to remove it: \
                     {rollback}"
                )
            })?;
            Err(error)
        }
    }
}

async fn resume_session(
    runtime: &ChatRuntime,
    chat: &mut Option<ChatDriver>,
    session: SessionId,
) -> Result<bool> {
    switch_session(runtime, chat, session, "resumed").await
}

async fn session_list(runtime: &ChatRuntime, active: Option<SessionId>) {
    print_event(
        "session",
        &format_session_list(runtime.list_sessions().await, active),
        EventStyle::Control,
    );
}

fn format_session_list(mut sessions: Vec<SessionId>, active: Option<SessionId>) -> String {
    sessions.sort_unstable();
    if sessions.is_empty() {
        return "available IDs: none".to_string();
    }

    let sessions = sessions
        .into_iter()
        .map(|session| {
            if Some(session) == active {
                format!("{session} (active)")
            } else {
                session.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join(", ");
    format!("available IDs: {sessions}")
}

async fn delete_session(
    runtime: &ChatRuntime,
    chat: &mut Option<ChatDriver>,
    requested: Option<SessionId>,
) -> Result<bool> {
    let active = chat.as_ref().map(|chat| chat.session);
    let session = requested
        .or(active)
        .ok_or_else(|| anyhow!("no active session; specify /session delete <session_id>"))?;
    if Some(session) != active {
        runtime.delete_session(session).await?;
        print_event(
            "session",
            &format!("deleted {session}"),
            EventStyle::Control,
        );
        return Ok(false);
    }

    let replacement = choose_replacement_session(session, runtime.list_sessions().await);
    let Some(replacement) = replacement else {
        runtime.delete_session(session).await?;
        *chat = None;
        print_event(
            "session",
            &format!("deleted {session}; next session will be created on first message"),
            EventStyle::Control,
        );
        return Ok(true);
    };

    let next = open_chat(runtime, replacement).await?;
    if let Err(error) = runtime.delete_session(session).await {
        drop(next);
        return Err(error.into());
    }

    *chat = Some(next);
    print_event(
        "session",
        &format!("deleted {session}; active {replacement}"),
        EventStyle::Control,
    );
    Ok(true)
}

fn choose_replacement_session(
    current: SessionId,
    sessions: impl IntoIterator<Item = SessionId>,
) -> Option<SessionId> {
    sessions
        .into_iter()
        .filter(|session| *session != current)
        .max()
}

fn session_command_allowed(state: ReplState, command: &str) -> bool {
    if state == ReplState::Idle {
        return true;
    }
    print_event(
        "error",
        &format!("cannot {command} while a turn is active; use /cancel first"),
        EventStyle::Error,
    );
    false
}

enum RenderOutcome {
    Continue,
    TurnStarted,
    InputRequested(InputRequestId),
    TurnEnded { user: bool, saw_output: bool },
    Closed,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum ReplState {
    #[default]
    Idle,
    Running,
    AwaitingInput(InputRequestId),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum StopRequest {
    Interrupt,
    Cancel,
}

impl StopRequest {
    fn label(self) -> &'static str {
        match self {
            Self::Interrupt => "interrupt",
            Self::Cancel => "cancel",
        }
    }

    fn message(self) -> &'static str {
        match self {
            Self::Interrupt => "turn interrupted",
            Self::Cancel => "turn cancelled",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CtrlCAction {
    Exit,
    Cancel,
}

fn ctrl_c_action(state: ReplState) -> CtrlCAction {
    match state {
        ReplState::Idle => CtrlCAction::Exit,
        ReplState::Running | ReplState::AwaitingInput(_) => CtrlCAction::Cancel,
    }
}

#[derive(Debug, PartialEq, Eq)]
enum UserTurnDisplay {
    AlreadyRendered,
    Deferred(String),
}

enum ReplActivity {
    Input(Option<LineInput>),
    Session(Option<Result<SessionEvent, SessionError>>),
    WaitingTick,
}

async fn next_session_event(
    chat: &mut Option<ChatDriver>,
) -> Option<Result<SessionEvent, SessionError>> {
    match chat {
        Some(chat) => chat.events.next().await,
        None => std::future::pending().await,
    }
}

fn active_chat(chat: &mut Option<ChatDriver>) -> Result<&mut ChatDriver> {
    chat.as_mut()
        .ok_or_else(|| anyhow!("REPL state requires an active session"))
}

async fn next_activity(
    input: impl Future<Output = Option<LineInput>>,
    event: impl Future<Output = Option<Result<SessionEvent, SessionError>>>,
    waiting_tick: impl Future<Output = ()>,
) -> ReplActivity {
    futures_lite::future::race(
        futures_lite::future::race(
            async move { ReplActivity::Input(input.await) },
            async move { ReplActivity::Session(event.await) },
        ),
        async move {
            waiting_tick.await;
            ReplActivity::WaitingTick
        },
    )
    .await
}

#[derive(Default)]
struct ContentRenderer {
    reasoning: LineState,
    output: LineState,
    above_prompt: bool,
}

impl ContentRenderer {
    fn start_turn(&mut self, above_prompt: bool, editor: &mut ChatLineEditor) -> Result<()> {
        self.finish(editor)?;
        self.above_prompt = above_prompt;
        Ok(())
    }

    fn abandon_live_render(&mut self) {
        self.reasoning.finish();
        self.output.finish();
    }

    fn reasoning(&mut self, part: StreamPart<String>, editor: &mut ChatLineEditor) -> Result<()> {
        match part {
            StreamPart::Delta(fragment) => self.reasoning_delta(&fragment, editor)?,
            StreamPart::End => self.finish_reasoning(editor, true)?,
        }
        Ok(())
    }

    fn output(&mut self, part: StreamPart<String>, editor: &mut ChatLineEditor) -> Result<bool> {
        match part {
            StreamPart::Delta(fragment) => {
                self.finish_reasoning(editor, true)?;
                self.output.observe(&fragment);
                if self.above_prompt {
                    editor.print_stream_fragment(&fragment)?;
                } else {
                    print!("{fragment}");
                    io::stdout().flush()?;
                }
                Ok(true)
            }
            StreamPart::End => {
                self.finish_output(editor);
                Ok(false)
            }
        }
    }

    fn tool_result(
        &mut self,
        part: StreamPart<(ToolCall, ToolOutput)>,
        editor: &mut ChatLineEditor,
    ) -> Result<()> {
        self.finish(editor)?;
        if let StreamPart::Delta((call, output)) = part {
            let status = if output.ok { "ok" } else { "failed" };
            self.event(
                editor,
                "tool",
                &format!("{}: {status}", call.name),
                EventStyle::Tools,
            )?;
        }
        Ok(())
    }

    fn event(
        &mut self,
        editor: &mut ChatLineEditor,
        label: &str,
        message: &str,
        style: EventStyle,
    ) -> Result<()> {
        if self.above_prompt {
            editor.print(format_event(label, message, style))?;
        } else {
            print_event(label, message, style);
        }
        Ok(())
    }

    fn reasoning_delta(&mut self, fragment: &str, editor: &mut ChatLineEditor) -> Result<()> {
        if fragment.is_empty() {
            return Ok(());
        }

        let style = EventStyle::Thinking.style();
        let starts_line = !self.reasoning.is_open();
        if self.above_prompt {
            let rendered = if starts_line {
                format!("  {style}{:<5}{style:#}  {fragment}", "think")
            } else {
                fragment.to_string()
            };
            editor.print_stream_fragment(&rendered)?;
        } else {
            if starts_line {
                eprint!("  {style}{:<5}{style:#}  ", "think");
            }
            eprint!("{fragment}");
            io::stderr().flush()?;
        }
        self.reasoning.observe(fragment);
        Ok(())
    }

    fn finish(&mut self, editor: &mut ChatLineEditor) -> Result<()> {
        if self.above_prompt {
            self.reasoning.finish();
            self.output.finish();
            editor.finish_stream();
        } else {
            finish_line(&mut self.reasoning, io::stderr());
            finish_line(&mut self.output, io::stdout());
        }
        Ok(())
    }

    fn finish_reasoning(
        &mut self,
        editor: &mut ChatLineEditor,
        continue_stream: bool,
    ) -> Result<()> {
        if self.above_prompt {
            if self.reasoning.finish() == Some(true) && continue_stream {
                editor.finish_stream_line()?;
            }
        } else {
            finish_line(&mut self.reasoning, io::stderr());
        }
        Ok(())
    }

    fn finish_output(&mut self, editor: &mut ChatLineEditor) {
        if self.above_prompt {
            self.output.finish();
            editor.finish_stream();
        } else {
            finish_line(&mut self.output, io::stdout());
        }
    }
}

#[derive(Default)]
struct LineState {
    open: bool,
    needs_newline: bool,
}

impl LineState {
    fn is_open(&self) -> bool {
        self.open
    }

    fn observe(&mut self, fragment: &str) {
        self.open = true;
        self.needs_newline = !fragment.ends_with('\n');
    }

    fn finish(&mut self) -> Option<bool> {
        if !self.open {
            return None;
        }
        let needs_newline = self.needs_newline;
        *self = Self::default();
        Some(needs_newline)
    }
}

fn finish_line(line: &mut LineState, mut writer: impl Write) {
    let Some(needs_newline) = line.finish() else {
        return;
    };
    if needs_newline {
        let _ = writeln!(writer);
    } else {
        let _ = writer.flush();
    }
}

enum EventStyle {
    Thinking,
    Tools,
    Permission,
    Control,
    Usage,
    Error,
}

impl EventStyle {
    fn style(&self) -> Style {
        if !io::stderr().is_terminal() {
            return Style::new();
        }

        match self {
            Self::Thinking => Style::new().dimmed().fg_color(Some(AnsiColor::Cyan.into())),
            Self::Tools => Style::new().bold().fg_color(Some(AnsiColor::Green.into())),
            Self::Permission | Self::Control => Style::new().fg_color(Some(AnsiColor::Cyan.into())),
            Self::Usage => Style::new()
                .dimmed()
                .fg_color(Some(AnsiColor::Yellow.into())),
            Self::Error => Style::new().bold().fg_color(Some(AnsiColor::Red.into())),
        }
    }
}

fn print_event(label: &str, message: &str, event_style: EventStyle) {
    eprintln!("{}", format_event(label, message, event_style));
}

fn print_above_or_below_prompt(
    editor: &mut ChatLineEditor,
    prompt_active: bool,
    label: &str,
    message: &str,
    style: EventStyle,
) -> Result<()> {
    if prompt_active {
        editor.print(format_event(label, message, style))
    } else {
        print_event(label, message, style);
        Ok(())
    }
}

fn print_user_prompt(
    editor: &mut ChatLineEditor,
    prompt_active: bool,
    message: &str,
) -> Result<()> {
    let rendered = format!("> {message}");
    if prompt_active {
        editor.print(rendered)
    } else {
        println!("{rendered}");
        Ok(())
    }
}

fn take_deferred_user_message(
    pending: &mut VecDeque<UserTurnDisplay>,
    origin: &TurnOrigin,
) -> Option<String> {
    if !matches!(origin, TurnOrigin::User) {
        return None;
    }
    match pending.pop_front() {
        Some(UserTurnDisplay::Deferred(message)) => Some(message),
        Some(UserTurnDisplay::AlreadyRendered) | None => None,
    }
}

fn format_event(label: &str, message: &str, event_style: EventStyle) -> String {
    let style = event_style.style();
    let mut lines = message.lines();
    let Some(first) = lines.next() else {
        return format!("  {style}{label:<5}{style:#}");
    };

    let mut rendered = format!("  {style}{label:<5}{style:#}  {first}");
    for line in lines {
        rendered.push_str("\n         ");
        rendered.push_str(line);
    }
    rendered
}

fn format_input_request(kind: &InputRequestKind) -> String {
    match kind {
        InputRequestKind::PermissionApproval { tool_call, reason } => format!(
            "Permission approval needed:\nTool call ID: {}\nTool: {}\nArguments: {}\nReason: {}\n\nReply with approval or rejection.",
            tool_call.id, tool_call.name, tool_call.arguments_json, reason
        ),
    }
}

fn format_usage(usage: ProviderUsage) -> String {
    fn value(value: Option<u64>) -> String {
        value.map_or_else(|| "-".to_string(), |count| count.to_string())
    }
    let rate = match (usage.input_tokens, usage.cache_read_tokens) {
        (Some(input), Some(cache_read)) if input > 0 => {
            format!("{:.2}%", cache_read as f64 / input as f64 * 100.0)
        }
        _ => "-".to_string(),
    };

    format!(
        "input={} output={} cache_read={} cache_write={} rate={}",
        value(usage.input_tokens),
        value(usage.output_tokens),
        value(usage.cache_read_tokens),
        value(usage.cache_write_tokens),
        rate,
    )
}

fn accumulate_usage(total: &mut ProviderUsage, usage: ProviderUsage) {
    fn accumulate(total: &mut Option<u64>, value: Option<u64>) {
        if let Some(value) = value {
            *total = Some(total.unwrap_or(0).saturating_add(value));
        }
    }

    accumulate(&mut total.input_tokens, usage.input_tokens);
    accumulate(&mut total.output_tokens, usage.output_tokens);
    accumulate(&mut total.cache_read_tokens, usage.cache_read_tokens);
    accumulate(&mut total.cache_write_tokens, usage.cache_write_tokens);
}

fn has_usage(usage: ProviderUsage) -> bool {
    usage.input_tokens.is_some()
        || usage.output_tokens.is_some()
        || usage.cache_read_tokens.is_some()
        || usage.cache_write_tokens.is_some()
}

async fn show_prompt(editor: &mut ChatLineEditor, prompt_active: &mut bool) -> Result<()> {
    if !*prompt_active {
        editor.show_prompt().await?;
        *prompt_active = true;
    }
    Ok(())
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let local = tokio::task::LocalSet::new();
    if let Err(error) = local.run_until(run()).await {
        print_event("error", &error.to_string(), EventStyle::Error);
        std::process::exit(1);
    }
}

async fn run() -> Result<()> {
    let crate_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    barracuda_agent_trace::init_logger(
        LevelFilter::Info,
        LogOutput::File(crate_dir.join("simulator_log.log")),
    )?;
    init_chat_tracing(&crate_dir.join("simulator_trace.log"))?;
    let env_path = crate_dir.join(".env.local");
    if env_path.is_file() {
        if let Err(error) = dotenvy::from_path(&env_path) {
            eprintln!("warning: failed to load {}: {error}", env_path.display());
        }
    }

    let persistence = RuntimeStorageConfig {
        persistence_root: MEMORY_DIR.to_string(),
        skill_roots: Vec::new(),
    };
    let mut llm_config = ModelApiConfig::new(
        BackendKind::OpenAiCompatible,
        required("BARRACUDA_LLM_API_KEY")?,
        required("BARRACUDA_LLM_MODEL")?,
        required("BARRACUDA_LLM_BASE_URL")?,
    );
    llm_config.timeout_ms = 60_000;
    let llm_factory = secure_llm_factory()?;
    let (runtime, service) =
        AgentRuntime::<DiskFs, TokioStack>::new(DiskFs::absolute(), persistence, llm_factory)?;
    let service_task = tokio::task::spawn_local(service);
    runtime.link_api(llm_config, ApiPurpose::RootAgent, true)?;
    runtime.start_all()?;
    let mut chat = None;

    eprintln!("Memory:  {MEMORY_DIR}");
    eprintln!("Session: none (created on first message)");
    eprintln!(
        "Type a message, or / for commands. Ctrl-C cancels an active turn or quits while idle.\n"
    );

    let mut editor = ChatLineEditor::new()?;
    let mut state = ReplState::Idle;
    let mut pending_stop = None;
    let mut pending_user_turns = VecDeque::new();
    let mut pending_settings = PendingSessionSettings::default();
    let mut total_usage = ProviderUsage::default();
    let mut prompt_active = false;
    let mut waiting_ticks = Ticker::every(WAITING_TICK);
    show_prompt(&mut editor, &mut prompt_active).await?;

    loop {
        let activity = next_activity(editor.next_input(), next_session_event(&mut chat), async {
            waiting_ticks.next().await;
            waiting_ticks.reset();
        })
        .await;
        match activity {
            ReplActivity::Input(Some(LineInput::Line(line))) => {
                editor.abandon_live_render(Some(&line))?;
                if let Some(chat) = chat.as_mut() {
                    chat.content.abandon_live_render();
                }
                prompt_active = false;
                let input = line.trim();
                if input.is_empty() {
                    break;
                }
                match parse_input(input) {
                    Ok(CliInput::Message(message)) => match state {
                        ReplState::Idle => {
                            let active = match ensure_lazy_session(&runtime, &mut chat).await {
                                Ok(active) => active,
                                Err(error) => {
                                    print_event("error", &error.to_string(), EventStyle::Error);
                                    show_prompt(&mut editor, &mut prompt_active).await?;
                                    continue;
                                }
                            };
                            if !pending_settings.apply(active).await {
                                show_prompt(&mut editor, &mut prompt_active).await?;
                                continue;
                            }
                            if active.append(message).await {
                                pending_user_turns.push_back(UserTurnDisplay::AlreadyRendered);
                                state = ReplState::Running;
                                show_prompt(&mut editor, &mut prompt_active).await?;
                                editor.start_waiting()?;
                                waiting_ticks.reset();
                            } else {
                                show_prompt(&mut editor, &mut prompt_active).await?;
                            }
                        }
                        ReplState::AwaitingInput(request) => {
                            if active_chat(&mut chat)?.respond(request, message).await {
                                state = ReplState::Running;
                                show_prompt(&mut editor, &mut prompt_active).await?;
                                editor.start_waiting()?;
                                waiting_ticks.reset();
                            } else {
                                show_prompt(&mut editor, &mut prompt_active).await?;
                            }
                        }
                        ReplState::Running => {
                            print_event(
                                "error",
                                "turn is running; use /append <message> to queue another turn",
                                EventStyle::Error,
                            );
                            show_prompt(&mut editor, &mut prompt_active).await?;
                        }
                    },
                    Ok(CliInput::Append(message)) => {
                        let was_idle = state == ReplState::Idle;
                        let active = match ensure_lazy_session(&runtime, &mut chat).await {
                            Ok(active) => active,
                            Err(error) => {
                                print_event("error", &error.to_string(), EventStyle::Error);
                                show_prompt(&mut editor, &mut prompt_active).await?;
                                continue;
                            }
                        };
                        if !pending_settings.apply(active).await {
                            show_prompt(&mut editor, &mut prompt_active).await?;
                            continue;
                        }
                        if active.append(message).await {
                            pending_user_turns
                                .push_back(UserTurnDisplay::Deferred(message.to_owned()));
                            print_event("append", "queued", EventStyle::Control);
                            if was_idle {
                                state = ReplState::Running;
                                show_prompt(&mut editor, &mut prompt_active).await?;
                                editor.start_waiting()?;
                                waiting_ticks.reset();
                            }
                        } else {
                            show_prompt(&mut editor, &mut prompt_active).await?;
                            continue;
                        }
                        if !was_idle {
                            show_prompt(&mut editor, &mut prompt_active).await?;
                        }
                    }
                    Ok(CliInput::Interrupt) => {
                        if state == ReplState::Idle {
                            print_event("error", "no active turn to interrupt", EventStyle::Error);
                            show_prompt(&mut editor, &mut prompt_active).await?;
                        } else if active_chat(&mut chat)?.interrupt().await
                            && pending_stop != Some(StopRequest::Cancel)
                        {
                            pending_stop = Some(StopRequest::Interrupt);
                        }
                        show_prompt(&mut editor, &mut prompt_active).await?;
                    }
                    Ok(CliInput::Cancel) => {
                        if state == ReplState::Idle {
                            print_event("error", "no active turn to cancel", EventStyle::Error);
                        } else if active_chat(&mut chat)?.cancel().await {
                            pending_stop = Some(StopRequest::Cancel);
                        }
                        show_prompt(&mut editor, &mut prompt_active).await?;
                    }
                    Ok(CliInput::SessionNew(persistence)) => {
                        if session_command_allowed(state, "create a session") {
                            match new_session(&runtime, &mut chat, persistence).await {
                                Ok(true) => {
                                    pending_stop = None;
                                    pending_user_turns.clear();
                                    let _ = pending_settings.apply(active_chat(&mut chat)?).await;
                                }
                                Ok(false) => {}
                                Err(error) => {
                                    print_event("error", &error.to_string(), EventStyle::Error);
                                }
                            }
                        }
                        show_prompt(&mut editor, &mut prompt_active).await?;
                    }
                    Ok(CliInput::SessionResume(session)) => {
                        if session_command_allowed(state, "resume a session") {
                            match resume_session(&runtime, &mut chat, session).await {
                                Ok(true) => {
                                    pending_stop = None;
                                    pending_user_turns.clear();
                                    let _ = pending_settings.apply(active_chat(&mut chat)?).await;
                                }
                                Ok(false) => {}
                                Err(error) => {
                                    print_event("error", &error.to_string(), EventStyle::Error);
                                    session_list(&runtime, chat.as_ref().map(|chat| chat.session))
                                        .await;
                                }
                            }
                        }
                        show_prompt(&mut editor, &mut prompt_active).await?;
                    }
                    Ok(CliInput::SessionDelete(session)) => {
                        if session_command_allowed(state, "delete a session") {
                            match delete_session(&runtime, &mut chat, session).await {
                                Ok(true) => {
                                    pending_stop = None;
                                    pending_user_turns.clear();
                                }
                                Ok(false) => {}
                                Err(error) => {
                                    print_event("error", &error.to_string(), EventStyle::Error);
                                }
                            }
                        }
                        show_prompt(&mut editor, &mut prompt_active).await?;
                    }
                    Ok(CliInput::SetPermission(level)) => {
                        if let Some(chat) = chat.as_ref() {
                            chat.set_permission_level(level).await;
                        } else {
                            pending_settings.set_permission(level);
                        }
                        show_prompt(&mut editor, &mut prompt_active).await?;
                    }
                    Ok(CliInput::SetReasoningEffort(effort)) => {
                        if let Some(chat) = chat.as_ref() {
                            chat.set_reasoning_effort(effort).await;
                        } else {
                            pending_settings.set_reasoning_effort(effort);
                        }
                        show_prompt(&mut editor, &mut prompt_active).await?;
                    }
                    Err(error) => {
                        let show_session_list = error.should_show_session_list();
                        print_event("error", &error.to_string(), EventStyle::Error);
                        if show_session_list {
                            session_list(&runtime, chat.as_ref().map(|chat| chat.session)).await;
                        }
                        show_prompt(&mut editor, &mut prompt_active).await?;
                    }
                }
            }
            ReplActivity::Input(Some(LineInput::Interrupted)) => {
                editor.abandon_live_render(None)?;
                if let Some(chat) = chat.as_mut() {
                    chat.content.abandon_live_render();
                }
                prompt_active = false;
                match ctrl_c_action(state) {
                    CtrlCAction::Exit => break,
                    CtrlCAction::Cancel => {
                        if active_chat(&mut chat)?.cancel().await {
                            pending_stop = Some(StopRequest::Cancel);
                        }
                        show_prompt(&mut editor, &mut prompt_active).await?;
                    }
                }
            }
            ReplActivity::Input(Some(LineInput::Eof) | None) => {
                editor.abandon_live_render(None)?;
                prompt_active = false;
                break;
            }
            ReplActivity::Input(Some(LineInput::Failed(error))) => {
                editor.abandon_live_render(None)?;
                return Err(error.into());
            }
            ReplActivity::Input(Some(LineInput::PromptReady)) => {
                prompt_active = true;
            }
            ReplActivity::Session(Some(Ok(event))) => {
                if let SessionEvent::Turn(TurnEvent::Started { origin, .. }) = &event {
                    show_prompt(&mut editor, &mut prompt_active).await?;
                    if let Some(message) =
                        take_deferred_user_message(&mut pending_user_turns, origin)
                    {
                        print_user_prompt(&mut editor, prompt_active, &message)?;
                    }
                    editor.start_waiting()?;
                    waiting_ticks.reset();
                }
                match active_chat(&mut chat)?.render(
                    event,
                    &mut editor,
                    prompt_active,
                    &mut total_usage,
                )? {
                    RenderOutcome::Continue => {}
                    RenderOutcome::TurnStarted => state = ReplState::Running,
                    RenderOutcome::InputRequested(request) => {
                        state = ReplState::AwaitingInput(request);
                        show_prompt(&mut editor, &mut prompt_active).await?;
                    }
                    RenderOutcome::TurnEnded { user, saw_output } => {
                        state = ReplState::Idle;
                        if let Some(stop) = pending_stop.take() {
                            print_above_or_below_prompt(
                                &mut editor,
                                prompt_active,
                                stop.label(),
                                stop.message(),
                                EventStyle::Error,
                            )?;
                        } else if user && !saw_output {
                            if prompt_active {
                                editor.print("(no reply)".to_string())?;
                            } else {
                                println!("\n(no reply)\n");
                            }
                        } else {
                            editor.clear_waiting()?;
                        }
                        show_prompt(&mut editor, &mut prompt_active).await?;
                    }
                    RenderOutcome::Closed => {
                        editor.clear_waiting()?;
                        break;
                    }
                }
            }
            ReplActivity::Session(Some(Err(error))) => {
                editor.clear_waiting()?;
                return Err(error.into());
            }
            ReplActivity::Session(None) => break,
            ReplActivity::WaitingTick => editor.advance_waiting()?,
        }
    }

    editor.clear_waiting()?;
    if has_usage(total_usage) {
        if prompt_active {
            editor.print(format_event(
                "total",
                &format_usage(total_usage),
                EventStyle::Usage,
            ))?;
        } else {
            eprintln!("\n");
            print_event("total", &format_usage(total_usage), EventStyle::Usage);
        }
    }
    if prompt_active {
        editor.print("Goodbye.".to_string())?;
    } else {
        eprintln!("Goodbye.");
    }
    runtime.shutdown().await;
    let _ = service_task.await;
    Ok(())
}

fn init_chat_tracing(path: &Path) -> Result<()> {
    let subscriber = FlatTreeSubscriber::with_sink(ChatTraceSink {
        file: Mutex::new(File::create(path)?),
    })
    .with_allowed_target_prefix("barracuda")
    .with_context_group_keys("run", ["system", "session", "turn", "agent", "iteration"]);
    tracing::subscriber::set_global_default(subscriber)?;
    Ok(())
}

/// Read a required, non-empty environment variable or fail with a clear message.
fn required(key: &str) -> Result<String> {
    match std::env::var(key) {
        Ok(value) if !value.is_empty() => Ok(value),
        _ => bail!("{key} must be set (in env or barracuda-cli/.env.local)"),
    }
}

fn resolve_ca_bundle_path_from(
    barracuda_bundle: Option<PathBuf>,
    ssl_cert_file: Option<PathBuf>,
    system_candidates: &[PathBuf],
) -> Result<PathBuf> {
    barracuda_bundle
        .filter(|path| !path.as_os_str().is_empty())
        .or_else(|| ssl_cert_file.filter(|path| !path.as_os_str().is_empty()))
        .or_else(|| {
            system_candidates
                .iter()
                .find(|path| path.is_file())
                .cloned()
        })
        .ok_or_else(|| {
            anyhow!("no system PEM CA bundle found; set BARRACUDA_TLS_CA_BUNDLE or SSL_CERT_FILE")
        })
}

fn secure_llm_factory() -> Result<ModelApiFactory<TokioStack>> {
    let system_candidates: Vec<PathBuf> = SYSTEM_CA_BUNDLE_CANDIDATES
        .iter()
        .map(PathBuf::from)
        .collect();
    let ca_path = resolve_ca_bundle_path_from(
        std::env::var_os("BARRACUDA_TLS_CA_BUNDLE").map(PathBuf::from),
        std::env::var_os("SSL_CERT_FILE").map(PathBuf::from),
        &system_candidates,
    )?;
    let mut ca_pem = std::fs::read(&ca_path)
        .map_err(|error| anyhow!("failed to read CA bundle {}: {error}", ca_path.display()))?;
    if !ca_pem.ends_with(&[0]) {
        ca_pem.push(0);
    }
    let ca_cstr = CStr::from_bytes_with_nul(&ca_pem)
        .map_err(|error| anyhow!("CA bundle must contain no interior NUL bytes: {error}"))?;
    let certificate = Certificate::new(X509::PEM(ca_cstr))
        .map_err(|error| anyhow!("failed to parse CA bundle: {error:?}"))?;
    let rng = TLS_RNG.init(SystemRng::open()?);
    let tls = Tls::new(rng).map_err(|error| anyhow!("failed to initialize mbedTLS: {error:?}"))?;
    let tls = TLS_RUNTIME
        .try_init(tls)
        .ok_or_else(|| anyhow!("mbedTLS runtime is already initialized"))?;
    let tls_reference = tls.reference();
    Ok(ModelApiFactory::new(move || {
        let tls = TlsConfig::new(TlsVersion::Tls1_2, certificate.clone(), None, tls_reference);
        ModelApi::new_with_tls(&NETWORK, tls, 16 * 1024, 8 * 1024)
    }))
}

#[cfg(test)]
mod tests {
    use barracuda_agent_runtime::RuntimeService;
    use tempdir::TempDir;

    use super::*;

    type ChatService = RuntimeService<DiskFs, TokioStack>;

    fn test_system(root: &TempDir) -> (ChatRuntime, ChatService) {
        let persistence = RuntimeStorageConfig {
            persistence_root: root.path().to_string_lossy().into_owned(),
            skill_roots: Vec::new(),
        };
        let llm_factory = ModelApiFactory::new(|| ModelApi::new(&NETWORK, 1024, 1024));
        let (runtime, service) =
            ChatRuntime::new(DiskFs::absolute(), persistence, llm_factory).expect("agent runtime");
        runtime.start_all().expect("start tools");
        (runtime, service)
    }

    #[test]
    fn ca_bundle_falls_back_to_an_existing_system_path() {
        let root = TempDir::new("barracuda-cli-ca").expect("temp dir");
        let missing = root.path().join("missing.pem");
        let available = root.path().join("system-ca.pem");
        std::fs::write(&available, b"test CA").expect("write CA fixture");

        let selected = resolve_ca_bundle_path_from(None, None, &[missing, available.clone()])
            .expect("discover system CA bundle");

        assert_eq!(selected, available);
    }

    #[test]
    fn empty_ca_environment_values_do_not_disable_system_discovery() {
        let root = TempDir::new("barracuda-cli-empty-ca-env").expect("temp dir");
        let available = root.path().join("system-ca.pem");
        std::fs::write(&available, b"test CA").expect("write CA fixture");

        let selected = resolve_ca_bundle_path_from(
            Some(PathBuf::new()),
            Some(PathBuf::new()),
            &[available.clone()],
        )
        .expect("ignore empty environment values");

        assert_eq!(selected, available);
    }

    #[test]
    fn explicit_ca_bundle_takes_precedence_over_system_discovery() {
        let explicit = PathBuf::from("explicit-ca.pem");
        let selected = resolve_ca_bundle_path_from(
            Some(explicit.clone()),
            Some(PathBuf::from("ssl-cert-file.pem")),
            &[PathBuf::from("system-ca.pem")],
        )
        .expect("select explicit CA bundle");

        assert_eq!(selected, explicit);
    }

    #[test]
    fn missing_ca_bundle_reports_how_to_override_discovery() {
        let error = resolve_ca_bundle_path_from(None, None, &[]).expect_err("missing CA bundle");

        assert!(error.to_string().contains("BARRACUDA_TLS_CA_BUNDLE"));
    }

    #[test]
    fn usage_line_includes_provider_cache_counters() {
        let usage = ProviderUsage {
            input_tokens: Some(128),
            output_tokens: Some(9),
            cache_read_tokens: Some(96),
            cache_write_tokens: None,
        };

        assert_eq!(
            format_usage(usage),
            "input=128 output=9 cache_read=96 cache_write=- rate=75.00%"
        );
    }

    #[test]
    fn usage_line_omits_rate_when_input_is_unavailable() {
        let usage = ProviderUsage {
            input_tokens: None,
            output_tokens: Some(9),
            cache_read_tokens: Some(96),
            cache_write_tokens: None,
        };

        assert_eq!(
            format_usage(usage),
            "input=- output=9 cache_read=96 cache_write=- rate=-"
        );
    }

    #[test]
    fn usage_totals_sum_iterations_and_recompute_rate() {
        let mut total = ProviderUsage::default();
        accumulate_usage(
            &mut total,
            ProviderUsage {
                input_tokens: Some(100),
                output_tokens: Some(10),
                cache_read_tokens: Some(80),
                cache_write_tokens: None,
            },
        );
        accumulate_usage(
            &mut total,
            ProviderUsage {
                input_tokens: Some(300),
                output_tokens: Some(20),
                cache_read_tokens: Some(120),
                cache_write_tokens: Some(50),
            },
        );

        assert_eq!(
            format_usage(total),
            "input=400 output=30 cache_read=200 cache_write=50 rate=50.00%"
        );
        assert!(has_usage(total));
        assert!(!has_usage(ProviderUsage::default()));
    }

    #[test]
    fn idle_repl_receives_session_events_without_waiting_for_stdin() {
        let event = SessionEvent::Turn(TurnEvent::Started {
            turn: barracuda_agent_runtime::TurnId(7),
            origin: TurnOrigin::ToolCall {
                call: ToolCall {
                    id: "call-3".to_owned(),
                    name: "background_work".to_owned(),
                    arguments_json: "{}".to_owned(),
                },
            },
        });

        let activity = futures_lite::future::block_on(next_activity(
            std::future::pending(),
            std::future::ready(Some(Ok(event))),
            std::future::pending(),
        ));

        assert!(matches!(
            activity,
            ReplActivity::Session(Some(Ok(SessionEvent::Turn(TurnEvent::Started {
                turn: barracuda_agent_runtime::TurnId(7),
                origin: TurnOrigin::ToolCall { .. },
            }))))
        ));
    }

    #[test]
    fn repl_accepts_input_while_session_events_are_pending() {
        let activity = futures_lite::future::block_on(next_activity(
            std::future::ready(Some(LineInput::Line("/interrupt".to_owned()))),
            std::future::pending(),
            std::future::pending(),
        ));

        assert!(matches!(
            activity,
            ReplActivity::Input(Some(LineInput::Line(line))) if line == "/interrupt"
        ));
    }

    #[test]
    fn repl_accepts_first_input_without_an_active_session() {
        let mut chat = None;
        let activity = futures_lite::future::block_on(next_activity(
            std::future::ready(Some(LineInput::Line("hello".to_owned()))),
            next_session_event(&mut chat),
            std::future::pending(),
        ));

        assert!(matches!(
            activity,
            ReplActivity::Input(Some(LineInput::Line(line))) if line == "hello"
        ));
        assert!(chat.is_none());
    }

    #[test]
    fn repl_emits_waiting_ticks() {
        let activity = futures_lite::future::block_on(next_activity(
            std::future::pending(),
            std::future::pending(),
            std::future::ready(()),
        ));

        assert!(matches!(activity, ReplActivity::WaitingTick));
    }

    #[test]
    fn permission_request_rendering_belongs_to_the_cli() {
        assert_eq!(
            format_input_request(&InputRequestKind::PermissionApproval {
                tool_call: barracuda_agent_runtime::ToolCall {
                    id: "call-1".to_owned(),
                    name: "skill_reload".to_owned(),
                    arguments_json: r#"{"name":"demo"}"#.to_owned(),
                },
                reason: "'skill_reload' is a High-risk action and needs approval.".to_owned(),
            }),
            "Permission approval needed:\nTool call ID: call-1\nTool: skill_reload\nArguments: {\"name\":\"demo\"}\nReason: 'skill_reload' is a High-risk action and needs approval.\n\nReply with approval or rejection."
        );
    }

    #[test]
    fn stop_requests_have_explicit_terminal_statuses() {
        assert_eq!(StopRequest::Interrupt.label(), "interrupt");
        assert_eq!(StopRequest::Interrupt.message(), "turn interrupted");
        assert_eq!(StopRequest::Cancel.label(), "cancel");
        assert_eq!(StopRequest::Cancel.message(), "turn cancelled");
    }

    #[test]
    fn ctrl_c_exits_only_while_idle() {
        assert_eq!(ctrl_c_action(ReplState::Idle), CtrlCAction::Exit);
        assert_eq!(ctrl_c_action(ReplState::Running), CtrlCAction::Cancel);
        assert_eq!(
            ctrl_c_action(ReplState::AwaitingInput(InputRequestId(7))),
            CtrlCAction::Cancel
        );
    }

    #[test]
    fn session_commands_are_allowed_only_while_idle() {
        assert!(session_command_allowed(ReplState::Idle, "switch sessions"));
        assert!(!session_command_allowed(
            ReplState::Running,
            "switch sessions"
        ));
        assert!(!session_command_allowed(
            ReplState::AwaitingInput(InputRequestId(3)),
            "switch sessions"
        ));
    }

    #[test]
    fn current_session_is_excluded_from_delete_replacement() {
        assert_eq!(
            choose_replacement_session(
                SessionId(4),
                [SessionId(1), SessionId(4), SessionId(7), SessionId(3)]
            ),
            Some(SessionId(7))
        );
        assert_eq!(
            choose_replacement_session(SessionId(4), [SessionId(4)]),
            None
        );
    }

    #[test]
    fn session_list_sorts_ids_and_marks_the_active_session() {
        assert_eq!(
            format_session_list(
                vec![SessionId(43), SessionId(41), SessionId(42)],
                Some(SessionId(42))
            ),
            "available IDs: session-41, session-42 (active), session-43"
        );
        assert_eq!(format_session_list(Vec::new(), None), "available IDs: none");
        assert_eq!(
            format_session_list(vec![SessionId(2), SessionId(1)], None),
            "available IDs: session-1, session-2"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn session_lifecycle_helpers_switch_and_replace_real_sessions() {
        tokio::task::LocalSet::new()
            .run_until(async {
                let root =
                    TempDir::new("barracuda-cli-session-lifecycle").expect("temporary directory");
                let (runtime, service) = test_system(&root);
                let service_task = tokio::task::spawn_local(service);

                let existing = runtime
                    .new_session(SessionPersistence::Persistent)
                    .await
                    .expect("existing session");
                let mut chat = None;
                assert!(resume_session(&runtime, &mut chat, existing)
                    .await
                    .expect("resume without active session"));
                assert_eq!(chat.as_ref().map(|chat| chat.session), Some(existing));
                assert_eq!(runtime.list_sessions().await, vec![existing]);
                active_chat(&mut chat)
                    .expect("active")
                    .control
                    .close()
                    .await
                    .expect("close existing");
                chat = None;
                runtime
                    .delete_session(existing)
                    .await
                    .expect("delete existing session");

                assert!(runtime.list_sessions().await.is_empty());
                assert!(delete_session(&runtime, &mut chat, None).await.is_err());
                assert!(runtime.list_sessions().await.is_empty());

                let first = ensure_lazy_session(&runtime, &mut chat)
                    .await
                    .expect("lazy session")
                    .session;
                assert_eq!(runtime.list_sessions().await, vec![first]);

                assert!(
                    new_session(&runtime, &mut chat, SessionPersistenceArg::Ephemeral)
                        .await
                        .expect("new and switch")
                );
                let second = chat.as_ref().map(|chat| chat.session).expect("active");
                assert_ne!(first, second);

                assert!(resume_session(&runtime, &mut chat, SessionId(999))
                    .await
                    .is_err());
                assert_eq!(chat.as_ref().map(|chat| chat.session), Some(second));

                assert!(resume_session(&runtime, &mut chat, first)
                    .await
                    .expect("resume first"));
                assert_eq!(chat.as_ref().map(|chat| chat.session), Some(first));

                assert!(delete_session(&runtime, &mut chat, Some(SessionId(999)))
                    .await
                    .is_err());
                assert_eq!(chat.as_ref().map(|chat| chat.session), Some(first));

                let third = runtime
                    .new_session(SessionPersistence::Persistent)
                    .await
                    .expect("third session");
                assert!(!delete_session(&runtime, &mut chat, Some(third))
                    .await
                    .expect("delete inactive"));
                assert_eq!(chat.as_ref().map(|chat| chat.session), Some(first));
                assert!(!runtime.list_sessions().await.contains(&third));

                assert!(delete_session(&runtime, &mut chat, None)
                    .await
                    .expect("delete active"));
                assert_eq!(chat.as_ref().map(|chat| chat.session), Some(second));
                assert!(!runtime.list_sessions().await.contains(&first));

                assert!(delete_session(&runtime, &mut chat, None)
                    .await
                    .expect("delete last active"));
                assert!(chat.is_none());
                assert!(runtime.list_sessions().await.is_empty());

                let replacement = ensure_lazy_session(&runtime, &mut chat)
                    .await
                    .expect("recreate lazily")
                    .session;
                assert_ne!(replacement, second);
                assert_eq!(runtime.list_sessions().await, vec![replacement]);
                runtime.shutdown().await;
                service_task.await.expect("service task");
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn session_new_honors_persistence_across_runtime_rebuilds() {
        tokio::task::LocalSet::new()
            .run_until(async {
                let root =
                    TempDir::new("barracuda-cli-session-persistence").expect("temporary directory");
                let (ephemeral, persistent) = {
                    let (runtime, service) = test_system(&root);
                    let service_task = tokio::task::spawn_local(service);
                    let mut chat = None;

                    assert!(
                        new_session(&runtime, &mut chat, SessionPersistenceArg::Ephemeral)
                            .await
                            .expect("new ephemeral")
                    );
                    let ephemeral = active_chat(&mut chat).expect("active ephemeral").session;

                    assert!(
                        new_session(&runtime, &mut chat, SessionPersistenceArg::Persistent)
                            .await
                            .expect("new persistent")
                    );
                    let persistent = active_chat(&mut chat).expect("active persistent").session;
                    active_chat(&mut chat)
                        .expect("active persistent")
                        .control
                        .close()
                        .await
                        .expect("close persistent");

                    runtime.shutdown().await;
                    service_task.await.expect("service task");
                    (ephemeral, persistent)
                };

                let (rebuilt, service) = test_system(&root);
                let service_task = tokio::task::spawn_local(service);
                assert!(!rebuilt.list_sessions().await.contains(&ephemeral));
                assert_eq!(rebuilt.list_sessions().await, vec![persistent]);
                rebuilt.shutdown().await;
                service_task.await.expect("service task");
            })
            .await;
    }

    #[test]
    fn deferred_user_messages_render_only_for_their_user_turn() {
        let mut pending = VecDeque::from([
            UserTurnDisplay::AlreadyRendered,
            UserTurnDisplay::Deferred("queued message".to_owned()),
        ]);
        let tool_origin = TurnOrigin::ToolCall {
            call: ToolCall {
                id: "call-4".to_owned(),
                name: "background_work".to_owned(),
                arguments_json: "{}".to_owned(),
            },
        };

        assert_eq!(take_deferred_user_message(&mut pending, &tool_origin), None);
        assert_eq!(pending.len(), 2);
        assert_eq!(
            take_deferred_user_message(&mut pending, &TurnOrigin::User),
            None
        );
        assert_eq!(
            take_deferred_user_message(&mut pending, &TurnOrigin::User),
            Some("queued message".to_owned())
        );
        assert!(pending.is_empty());
    }
}
