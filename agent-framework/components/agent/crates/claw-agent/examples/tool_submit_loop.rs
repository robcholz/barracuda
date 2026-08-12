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
//! cargo run -p claw-agent --example tool_submit_loop \
//!   --target x86_64-unknown-linux-gnu
//! ```

use claw_agent::{
    stream::StreamPart,
    tools::{EmptyArgs, Tool, ToolFuture, ToolGroup, ToolHandler, ToolOutput, ToolSpec},
    AgentSystem, ApiPurpose, BackendKind, ClawApiConfig, ClawApiFactory, IterationEvent, Message,
    SessionEvent, SessionPersistence, TurnEvent,
};
use claw_api::ClawApi;
use claw_fs::MemFs;
use claw_log::{LevelFilter, LogOutput, TracingConfig};
use claw_net::testing::{ScriptStep, ScriptedStack};
use futures_lite::StreamExt;
use static_cell::StaticCell;

static NETWORK: StaticCell<ScriptedStack> = StaticCell::new();

/// A tool: returns a fixed timestamp. Registering it makes `time_now`
/// resolvable by the agent; whether the model calls it is up to the prompt.
struct TimeNowTool;

impl ToolSpec for TimeNowTool {
    fn name(&self) -> &str {
        "time_now"
    }

    fn schema(&self) -> &str {
        include_str!("time_now.schema.json")
    }

    fn arguments_validator(&self) -> &'static json_validator::Validator {
        const VALIDATOR: json_validator::Validator =
            json_validator::validator!("examples/time_now.schema.json");
        &VALIDATOR
    }
}

impl ToolHandler for TimeNowTool {
    type Args = EmptyArgs;

    fn invoke<'a>(&'a self, _args: Self::Args) -> ToolFuture<'a> {
        Box::pin(async {
            Ok(ToolOutput {
                content: "2026-06-29T17:00:00Z".into(),
                ok: true,
            })
        })
    }
}

/// A test LLM config; its base URL is never dialed (HTTP is the scripted double).
fn scripted_llm() -> ClawApiConfig {
    ClawApiConfig::new(
        BackendKind::OpenAiCompatible,
        "sk-example",
        "gpt-example",
        "http://example.invalid",
    )
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> anyhow::Result<()> {
    tokio::task::LocalSet::new().run_until(run()).await
}

async fn run() -> anyhow::Result<()> {
    claw_log::init_logger(LevelFilter::Info, LogOutput::Stderr)?;
    claw_log::init_tracing(
        TracingConfig::default()
            .with_context_group_keys("run", ["system", "session", "turn", "agent", "iteration"]),
    )?;

    // 1. Build the system. Hermetic backends (in-memory fs + scripted LLM) keep
    //    the example offline and deterministic.
    let event = serde_json::json!({
        "choices": [{
            "delta": {
                "content": "Hello from the agent — the local time is 2026-06-29T17:00:00Z."
            }
        }]
    });
    let sse = format!("data: {event}\n\ndata: [DONE]\n\n");
    let network: &'static ScriptedStack =
        NETWORK.init(ScriptedStack::new([ScriptStep::sse(200, &[sse.as_str()])]));
    let llm_factory = ClawApiFactory::new(move || ClawApi::new(network, 4096, 1024));

    let (system, service) = AgentSystem::<MemFs, ScriptedStack>::with_tool_groups(
        MemFs::new(),
        claw_agent::AgentPersistenceConfig {
            persistence_root: "/mem".to_string(),
            skill_roots: Vec::new(),
        },
        llm_factory,
        [ToolGroup::new("example", true, [Tool::new(TimeNowTool)])],
    )?;
    let service_task = tokio::task::spawn_local(service);
    system.link_api(scripted_llm(), ApiPurpose::RootAgent, true)?;
    println!("registered tool `time_now`");
    system.start_all()?;
    let session = system.new_session(SessionPersistence::Persistent).await?;

    // 2. Drive the loop: explicit session id selects the agent session.
    let (control, mut events) = system.open_session(session).await?;
    control
        .append(Message::text("Hi, what time is it?"))
        .await?;

    println!("\nsession `{session}` events:");
    let mut outputs = Vec::new();
    while let Some(event) = events.next().await {
        let event = event?;
        match event {
            SessionEvent::Turn(TurnEvent::EffectOutput(StreamPart::Delta(text)))
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
            | SessionEvent::Turn(TurnEvent::EffectOutput(StreamPart::End)) => {}
            SessionEvent::Error(error) => println!("  [error] {error}"),
            SessionEvent::Turn(TurnEvent::Error(error)) => println!("  [error] {error}"),
            SessionEvent::Turn(TurnEvent::Ended { .. }) => break,
            other => println!("  [{other:?}]"),
        }
    }
    assert_eq!(outputs.len(), 1, "expected exactly one output");

    system.shutdown().await;
    let _ = service_task.await;
    Ok(())
}
