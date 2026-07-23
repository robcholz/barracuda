//! A full tool/session message loop, end to end and offline.
//!
//! This is the reference for how a device (firmware or host) drives the agent
//! through a session stream plus tools. Everything below uses the
//! `claw_agent` surface:
//!
//! 1. Build an [`AgentSystem`] with a **tool**.
//! 2. Start the registered runtime objects.
//! 3. Open a session, append user text, and read replies from the same stream.
//!
//! The LLM is a scripted in-memory double and the filesystem is in-memory, so the
//! example runs hermetically (no network, no API key):
//!
//! ```bash
//! cargo run -p claw-agent --features dev --example tool_submit_loop \
//!   --target x86_64-unknown-linux-gnu
//! ```

use claw_agent::{
    stream::StreamPart,
    tools::{SyncToolHandler, Tool, ToolGroup, ToolInvocation, ToolOutput, ToolResult, ToolSpec},
    AgentSystem, ApiPurpose, BackendKind, ClawApiConfig, IterationEvent, Message, SessionEvent,
    SessionPersistence, TurnEvent,
};
use claw_interface::http::SliceChunks;
use claw_interface::{
    BlockingHttpAdapter, Cancel, ClawHttp, HttpError, HttpJsonRequest, HttpResponseFuture,
    HttpStatusCode, ImmediateTimer, MemFs, SharedScriptHttp, StdThread, StreamingHttp,
    TokioExecutor,
};
use claw_log::{LevelFilter, LogOutput, TracingConfig};
use futures_lite::StreamExt;

#[derive(Default)]
struct Sse<T>(T);

impl<T: ClawHttp> ClawHttp for Sse<T> {
    fn post_json<'a>(
        &'a mut self,
        request: &'a HttpJsonRequest<'a>,
        cancel: Cancel<'a>,
    ) -> HttpResponseFuture<'a> {
        self.0.post_json(request, cancel)
    }
}

impl<T: ClawHttp> StreamingHttp for Sse<T> {
    type ByteStream<'a>
        = SliceChunks<'a>
    where
        Self: 'a;

    async fn post_json_streaming<'a, 'r>(
        &'a mut self,
        request: &'r HttpJsonRequest<'r>,
        cancel: Cancel<'a>,
    ) -> Result<(HttpStatusCode, Self::ByteStream<'a>), HttpError> {
        let response = self.0.post_json(request, cancel).await?;
        let message: serde_json::Value =
            serde_json::from_str(&response.body).map_err(|_| HttpError::Aborted)?;
        let content = message["choices"][0]["message"]["content"]
            .as_str()
            .unwrap_or_default();
        let event = serde_json::json!({
            "choices": [{ "delta": { "content": content } }]
        });
        let body = format!("data: {event}\n\ndata: [DONE]\n\n");
        Ok((
            response.status_code,
            SliceChunks::once_with_cancel(body.into_bytes(), cancel),
        ))
    }
}

/// A tool: returns a fixed timestamp. Registering it makes `time_now`
/// resolvable by the agent; whether the model calls it is up to the prompt.
struct TimeNowTool;

impl ToolSpec for TimeNowTool {
    fn name(&self) -> &str {
        "time_now"
    }

    fn schema(&self) -> &str {
        r#"{"type":"function","function":{"name":"time_now","description":"Current time","parameters":{"type":"object","properties":{}}}}"#
    }
}

impl SyncToolHandler for TimeNowTool {
    fn invoke(&self, _call: &ToolInvocation) -> ToolResult<ToolOutput> {
        Ok(ToolOutput {
            content: "2026-06-29T17:00:00Z".into(),
            ok: true,
        })
    }
}

/// A scripted assistant turn returning plain text (no tool call this round).
fn assistant_text(text: &str) -> String {
    serde_json::json!({
        "choices": [{ "message": { "role": "assistant", "content": text } }]
    })
    .to_string()
}

/// A test LLM config; its base URL is never dialed (HTTP is the scripted double).
fn scripted_llm() -> ClawApiConfig {
    ClawApiConfig::new(
        BackendKind::OpenAiCompatible,
        "sk-example",
        "gpt-example",
        "https://example.invalid",
    )
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    claw_log::init_logger(LevelFilter::Info, LogOutput::Stderr)?;
    claw_log::init_tracing(
        TracingConfig::default()
            .with_context_group_keys("run", ["system", "session", "turn", "agent", "iteration"]),
    )?;

    // 1. Build the system. Hermetic backends (in-memory fs + scripted LLM) keep
    //    the example offline and deterministic.
    SharedScriptHttp::install(vec![assistant_text(
        "Hello from the agent — the local time is 2026-06-29T17:00:00Z.",
    )]);

    let system = AgentSystem::<
        MemFs,
        Sse<BlockingHttpAdapter<SharedScriptHttp>>,
        ImmediateTimer,
    >::with_tool_groups::<StdThread, TokioExecutor>(
        claw_agent::AgentPersistenceConfig {
            persistence_root: "/mem".to_string(),
            skill_roots: Vec::new(),
        },
        [ToolGroup::new(
            "example",
            true,
            [Tool::from_sync(TimeNowTool)],
        )],
    )?;
    system.link_api(scripted_llm(), ApiPurpose::RootAgent, true)?;
    println!("registered tool `time_now`");
    system.start_all()?;
    let session = system.new_session(SessionPersistence::Persistent)?;

    // 2. Drive the loop: explicit session id selects the agent session.
    let (control, mut events) = system.open_session(session)?;
    control
        .append(Message::text("Hi, what time is it?"))
        .await?;

    println!("\nsession `{session}` events:");
    let mut outputs = Vec::new();
    while let Some(event) = events.next().await {
        let event = event?;
        match event {
            SessionEvent::Turn(TurnEvent::Output(StreamPart::Delta(text)))
            | SessionEvent::Turn(TurnEvent::Iteration(IterationEvent::Output(
                StreamPart::Delta(text),
            ))) => {
                println!("  > {text}");
                outputs.push(text);
            }
            SessionEvent::Turn(TurnEvent::Iteration(IterationEvent::Reasoning(
                StreamPart::Delta(text),
            ))) => println!("  [thinking] {text}"),
            SessionEvent::Turn(TurnEvent::Iteration(IterationEvent::ToolResult(
                StreamPart::Delta((call, output)),
            ))) => {
                println!(
                    "  [tool] {}: {}",
                    call.name,
                    if output.ok { "ok" } else { "failed" }
                )
            }
            SessionEvent::Turn(TurnEvent::Iteration(
                IterationEvent::Reasoning(StreamPart::End)
                | IterationEvent::Output(StreamPart::End)
                | IterationEvent::ToolResult(StreamPart::End),
            ))
            | SessionEvent::Turn(TurnEvent::Output(StreamPart::End)) => {}
            SessionEvent::Error(error) => println!("  [error] {error}"),
            SessionEvent::Turn(TurnEvent::Error(error)) => println!("  [error] {error}"),
            SessionEvent::Turn(TurnEvent::Ended { .. }) => break,
            other => println!("  [{other:?}]"),
        }
    }
    assert_eq!(outputs.len(), 1, "expected exactly one output");

    Ok(())
}
