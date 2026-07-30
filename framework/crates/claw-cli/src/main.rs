//! `claw-agent-chat` — a minimal REPL that drives the whole agent system through
//! the public [`claw_agent`] API: build an [`AgentSystem`], create a session,
//! append user text, and print each turn's replies.
//!
//! LLM config is read from this crate's `.env.local`:
//! `CLAW_LLM_API_KEY`, `CLAW_LLM_BASE_URL`, and `CLAW_LLM_MODEL`. Memory is
//! written under this crate's `output/claw-agent-chat/`.
//!
//! ```
//! cargo run -p claw-cli --features cache_profile --bin claw-agent-chat
//! ```
//!
mod command;
mod line_editor;

use std::collections::VecDeque;
use std::future::Future;
use std::io::{self, IsTerminal, Write};
use std::path::Path;
use std::time::Duration;

use anstyle::{AnsiColor, Style};
use anyhow::{bail, Result};
use claw_agent::{
    stream::StreamPart, AgentPersistenceConfig, AgentSystem, ApiPurpose, BackendKind,
    ClawApiConfig, InputRequestId, InputRequestKind, IterationEvent, Message, ProviderUsage,
    SessionControl, SessionError, SessionEvent, SessionPersistence, SessionStream, ToolCall,
    ToolOutput, TurnEvent, TurnOrigin,
};
use claw_interface::{DiskFs, RealHttp, StdThread, TokioExecutor, TokioTimer};
use claw_log::{LevelFilter, LogOutput, TracingConfig};
use futures_lite::StreamExt;

use command::{parse_input, CliInput, PermissionLevelArg, ReasoningEffortArg};
use line_editor::{ChatLineEditor, LineInput};

const MEMORY_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/output/claw-agent-chat");
const WAITING_TICK: Duration = Duration::from_millis(400);

struct ChatDriver {
    control: SessionControl,
    events: SessionStream,
    total_usage: ProviderUsage,
    content: ContentRenderer,
    active_origin: Option<TurnOrigin>,
    saw_output: bool,
}

impl ChatDriver {
    fn new(control: SessionControl, events: SessionStream) -> Self {
        Self {
            control,
            events,
            total_usage: ProviderUsage::default(),
            content: ContentRenderer::default(),
            active_origin: None,
            saw_output: false,
        }
    }

    fn total_usage(&self) -> Option<ProviderUsage> {
        has_usage(self.total_usage).then_some(self.total_usage)
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
                accumulate_usage(&mut self.total_usage, usage);
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

#[tokio::main]
async fn main() {
    if let Err(error) = run().await {
        print_event("error", &error.to_string(), EventStyle::Error);
        std::process::exit(1);
    }
}

async fn run() -> Result<()> {
    claw_log::init_logger(
        LevelFilter::Info,
        LogOutput::File(Path::new(env!("CARGO_MANIFEST_DIR")).join("simulator.log")),
    )?;
    claw_log::init_tracing(
        TracingConfig::default()
            .with_context_group_keys("run", ["system", "session", "turn", "agent", "iteration"]),
    )?;
    let env_path = Path::new(env!("CARGO_MANIFEST_DIR")).join(".env.local");
    if env_path.is_file() {
        if let Err(error) = dotenvy::from_path(&env_path) {
            eprintln!("warning: failed to load {}: {error}", env_path.display());
        }
    }

    let persistence = AgentPersistenceConfig {
        persistence_root: MEMORY_DIR.to_string(),
        skill_roots: Vec::new(),
    };
    let mut llm_config = ClawApiConfig::new(
        BackendKind::OpenAiCompatible,
        required("CLAW_LLM_API_KEY")?,
        required("CLAW_LLM_MODEL")?,
        required("CLAW_LLM_BASE_URL")?,
    );
    llm_config.timeout_ms = 60_000;
    let system =
        AgentSystem::<DiskFs, RealHttp, TokioTimer>::new::<StdThread, TokioExecutor>(persistence)?;
    system.link_api(llm_config, ApiPurpose::RootAgent, true)?;
    system.start_all()?;
    let session = system.new_session(SessionPersistence::Persistent)?;
    let (control, events) = system.open_session(session)?;
    let mut chat = ChatDriver::new(control, events);

    eprintln!("Memory:  {MEMORY_DIR}");
    eprintln!(
        "Type a message, or / for commands. Ctrl-C cancels an active turn or quits while idle.\n"
    );

    let mut editor = ChatLineEditor::new()?;
    let mut state = ReplState::Idle;
    let mut pending_stop = None;
    let mut pending_user_turns = VecDeque::new();
    let mut prompt_active = false;
    let mut waiting_ticks =
        tokio::time::interval_at(tokio::time::Instant::now() + WAITING_TICK, WAITING_TICK);
    waiting_ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    show_prompt(&mut editor, &mut prompt_active).await?;

    loop {
        let activity = next_activity(editor.next_input(), chat.events.next(), async {
            waiting_ticks.tick().await;
        })
        .await;
        match activity {
            ReplActivity::Input(Some(LineInput::Line(line))) => {
                editor.abandon_live_render(Some(&line))?;
                chat.content.abandon_live_render();
                prompt_active = false;
                let input = line.trim();
                if input.is_empty() {
                    break;
                }
                match parse_input(input) {
                    Ok(CliInput::Message(message)) => match state {
                        ReplState::Idle => {
                            if chat.append(message).await {
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
                            if chat.respond(request, message).await {
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
                        if chat.append(message).await {
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
                        } else if chat.interrupt().await
                            && pending_stop != Some(StopRequest::Cancel)
                        {
                            pending_stop = Some(StopRequest::Interrupt);
                        }
                        show_prompt(&mut editor, &mut prompt_active).await?;
                    }
                    Ok(CliInput::Cancel) => {
                        if state == ReplState::Idle {
                            print_event("error", "no active turn to cancel", EventStyle::Error);
                        } else if chat.cancel().await {
                            pending_stop = Some(StopRequest::Cancel);
                        }
                        show_prompt(&mut editor, &mut prompt_active).await?;
                    }
                    Ok(CliInput::SetPermission(level)) => {
                        chat.set_permission_level(level).await;
                        show_prompt(&mut editor, &mut prompt_active).await?;
                    }
                    Ok(CliInput::SetReasoningEffort(effort)) => {
                        chat.set_reasoning_effort(effort).await;
                        show_prompt(&mut editor, &mut prompt_active).await?;
                    }
                    Err(error) => {
                        print_event("error", &error.to_string(), EventStyle::Error);
                        show_prompt(&mut editor, &mut prompt_active).await?;
                    }
                }
            }
            ReplActivity::Input(Some(LineInput::Interrupted)) => {
                editor.abandon_live_render(None)?;
                chat.content.abandon_live_render();
                prompt_active = false;
                match ctrl_c_action(state) {
                    CtrlCAction::Exit => break,
                    CtrlCAction::Cancel => {
                        if chat.cancel().await {
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
                match chat.render(event, &mut editor, prompt_active)? {
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
    if let Some(usage) = chat.total_usage() {
        if prompt_active {
            editor.print(format_event(
                "total",
                &format_usage(usage),
                EventStyle::Usage,
            ))?;
        } else {
            eprintln!("\n");
            print_event("total", &format_usage(usage), EventStyle::Usage);
        }
    }
    if prompt_active {
        editor.print("Goodbye.".to_string())?;
    } else {
        eprintln!("Goodbye.");
    }
    Ok(())
}

/// Read a required, non-empty environment variable or fail with a clear message.
fn required(key: &str) -> Result<String> {
    match std::env::var(key) {
        Ok(value) if !value.is_empty() => Ok(value),
        _ => bail!("{key} must be set (in env or claw-cli/.env.local)"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
            turn: claw_agent::TurnId(7),
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
                turn: claw_agent::TurnId(7),
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
                tool_call: claw_agent::ToolCall {
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
