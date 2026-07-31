#![allow(clippy::unwrap_used)]

mod support;

use core::future::poll_fn;
use core::task::{Poll, Waker};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use claw_agent::tools::{
    AsyncToolHandler, Tool, ToolConfig, ToolFuture, ToolGroup, ToolInvocation, ToolOutput, ToolSpec,
};
use claw_agent::{
    stream::StreamPart, IterationEvent, Message, SessionEvent, TurnEvent, TurnId, TurnOrigin,
};
use claw_interface::{
    BlockingHttpAdapter, DiskFs, ImmediateTimer, SharedScriptHttp, StdThread, TokioExecutor,
};
use claw_log::{FlatTreeSubscriber, TraceSink};
use futures_lite::future::block_on;
use serde_json::json;
use support::{assistant_text, drain_until_turn_ended, llm_config, serialize_script, Sse};
use tempdir::TempDir;
use tracing::Level;

type TestSystem =
    claw_agent::AgentSystem<DiskFs, Sse<BlockingHttpAdapter<SharedScriptHttp>>, ImmediateTimer>;

#[derive(Clone, Default)]
struct RecordingSink(Arc<Mutex<Vec<String>>>);

impl RecordingSink {
    fn lines(&self) -> Vec<String> {
        self.0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }
}

impl TraceSink for RecordingSink {
    fn write_line(&self, _level: Level, _tag: &str, line: &str) {
        self.0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .push(line.to_owned());
    }
}

#[test]
fn detached_completion_opens_a_tool_call_turn_after_the_root_becomes_idle() {
    let sink = RecordingSink::default();
    let subscriber = FlatTreeSubscriber::with_sink(sink.clone())
        .with_allowed_target_prefix("claw")
        .with_context_group_keys("run", ["system", "session", "turn", "agent", "iteration"]);
    tracing::subscriber::set_global_default(subscriber)
        .expect("this single-test binary installs tracing exactly once");

    let _script = serialize_script();
    let temp = TempDir::new("detached-tool-turn").unwrap();
    let root = temp.path().to_string_lossy().into_owned();
    SharedScriptHttp::install(vec![
        assistant_tool_call("background_tool"),
        assistant_text("background accepted"),
        assistant_text("foreground reply"),
        assistant_text("background delivered"),
    ]);

    let gate = Arc::new(ToolGate::default());
    let system = TestSystem::with_tool_groups::<StdThread, TokioExecutor>(
        DiskFs::absolute(),
        claw_agent::AgentPersistenceConfig {
            persistence_root: root,
            skill_roots: Vec::new(),
        },
        [ToolGroup::new(
            "background",
            true,
            [Tool::from_async(BackgroundTool {
                gate: Arc::clone(&gate),
            })
            .with_config(ToolConfig { detached: true })],
        )],
    )
    .unwrap();
    system
        .link_api(llm_config(), claw_agent::ApiPurpose::RootAgent, true)
        .unwrap();
    system.start_all().unwrap();

    let session = system
        .new_session(claw_agent::SessionPersistence::Ephemeral)
        .unwrap();
    let (control, mut events) = system.open_session(session).unwrap();

    block_on(control.append(Message::text("start background work"))).unwrap();
    let first = drain_until_turn_ended(&mut events);
    assert_turn(&first, TurnId(1), |origin| {
        matches!(origin, TurnOrigin::User)
    });
    assert_eq!(outputs(&first), vec!["background accepted"]);
    wait_until(&gate.started, "detached tool was not polled");

    block_on(control.append(Message::text("stay responsive"))).unwrap();
    let second = drain_until_turn_ended(&mut events);
    assert_turn(&second, TurnId(2), |origin| {
        matches!(origin, TurnOrigin::User)
    });
    assert_eq!(outputs(&second), vec!["foreground reply"]);

    gate.release();
    let completed = drain_until_turn_ended(&mut events);
    assert_turn(&completed, TurnId(3), |origin| {
        matches!(
            origin,
            TurnOrigin::ToolCall { call }
                if call.id == "call-detached-1" && call.name == "background_tool"
        )
    });
    assert_eq!(outputs(&completed), vec!["background delivered"]);

    drop(control);
    drop(events);
    drop(system);

    assert_turn_trace(&sink.lines());
}

fn assistant_tool_call(name: &str) -> String {
    json!({
        "choices": [{
            "message": {
                "role": "assistant",
                "tool_calls": [{
                    "id": "call-detached-1",
                    "type": "function",
                    "function": {
                        "name": name,
                        "arguments": "{}",
                    },
                }],
            },
        }],
    })
    .to_string()
}

fn assert_turn(
    events: &[SessionEvent],
    turn: TurnId,
    origin_matches: impl FnOnce(&TurnOrigin) -> bool,
) {
    assert!(matches!(
        events.first(),
        Some(SessionEvent::Turn(TurnEvent::Started {
            turn: event_turn,
            origin,
        })) if *event_turn == turn && origin_matches(origin)
    ));
    assert!(matches!(
        events.last(),
        Some(SessionEvent::Turn(TurnEvent::Ended { turn: event_turn }))
            if *event_turn == turn
    ));
}

fn outputs(events: &[SessionEvent]) -> Vec<&str> {
    events
        .iter()
        .filter_map(|event| match event {
            SessionEvent::Turn(
                TurnEvent::EffectOutput(StreamPart::Delta(text))
                | TurnEvent::Iteration(IterationEvent::Output(StreamPart::Delta(text))),
            ) => Some(text.as_str()),
            _ => None,
        })
        .collect()
}

fn assert_turn_trace(lines: &[String]) {
    let turns = lines
        .iter()
        .filter(|line| line_type(line) == Some("enter") && token(line, "span-name") == Some("turn"))
        .collect::<Vec<_>>();
    assert_eq!(turns.len(), 3, "{}", lines.join("\n"));

    for (turn, (expected_id, expected_cause)) in turns.into_iter().zip([
        ("turn-1", "user"),
        ("turn-2", "user"),
        ("turn-3", "detached_tool"),
    ]) {
        assert_eq!(token(turn, "turn"), Some(expected_id), "{turn}");
        assert_eq!(token(turn, "cause"), Some(expected_cause), "{turn}");
        let turn_span = token(turn, "span").expect("turn span id");
        let agent = lines
            .iter()
            .find(|line| {
                line_type(line) == Some("enter")
                    && token(line, "span-name") == Some("agent")
                    && token(line, "parent") == Some(turn_span)
            })
            .unwrap_or_else(|| panic!("missing agent child for {turn}"));
        assert_eq!(token(agent, "turn"), Some(expected_id), "{agent}");
        assert!(lines.iter().any(|line| {
            line_type(line) == Some("exit") && token(line, "span") == Some(turn_span)
        }));
    }
}

fn token<'a>(line: &'a str, key: &str) -> Option<&'a str> {
    line.split(' ').find_map(|raw| {
        let token = raw.trim_matches(|ch| ch == '<' || ch == '>');
        token.strip_prefix(key)?.strip_prefix('=')
    })
}

fn line_type(line: &str) -> Option<&str> {
    line.split(' ').nth(2)
}

#[derive(Default)]
struct ToolGate {
    started: AtomicBool,
    released: AtomicBool,
    waker: Mutex<Option<Waker>>,
}

impl ToolGate {
    fn release(&self) {
        self.released.store(true, Ordering::SeqCst);
        if let Some(waker) = self
            .waker
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take()
        {
            waker.wake();
        }
    }
}

struct BackgroundTool {
    gate: Arc<ToolGate>,
}

impl ToolSpec for BackgroundTool {
    fn name(&self) -> &str {
        "background_tool"
    }

    fn schema(&self) -> &str {
        r#"{"type":"function","function":{"name":"background_tool"}}"#
    }
}

impl AsyncToolHandler for BackgroundTool {
    fn invoke<'a>(&'a self, _call: &'a ToolInvocation) -> ToolFuture<'a> {
        Box::pin(poll_fn(move |context| {
            self.gate.started.store(true, Ordering::SeqCst);
            if self.gate.released.load(Ordering::SeqCst) {
                return Poll::Ready(Ok(ToolOutput {
                    content: "background result".to_owned(),
                    ok: true,
                }));
            }
            *self
                .gate
                .waker
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(context.waker().clone());
            Poll::Pending
        }))
    }
}

fn wait_until(flag: &AtomicBool, failure: &str) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !flag.load(Ordering::SeqCst) {
        assert!(Instant::now() < deadline, "{failure}");
        thread::yield_now();
    }
}
