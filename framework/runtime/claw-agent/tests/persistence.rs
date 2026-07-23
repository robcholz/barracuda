#![allow(clippy::unwrap_used)]

mod support;

use core::future::poll_fn;
use core::task::{Poll, Waker};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use claw_agent::{AgentPersistenceConfig, AgentSystem, Message, SessionId, SessionPersistence};
use claw_interface::{
    BlockingHttpAdapter, ClawFs, DiskFs, ImmediateTimer, SharedScriptHttp, StdThread, TokioExecutor,
};
use claw_tool::{
    AsyncToolHandler, SyncToolHandler, Tool, ToolFuture, ToolGroup, ToolInvocation, ToolOutput,
    ToolResult, ToolSpec,
};
use futures_lite::future::block_on;
use futures_lite::StreamExt;
use serde_json::{json, Value};

use support::{install_script, llm_config, serialize_script, Sse};
use tempdir::TempDir;

type DiskSystem = AgentSystem<DiskFs, Sse<BlockingHttpAdapter<SharedScriptHttp>>, ImmediateTimer>;

#[test]
fn persistent_sessions_use_the_documented_layout_and_restore() {
    let _script = serialize_script();
    let temp = TempDir::new("persistence-layout").unwrap();
    let root = temp.path().to_string_lossy().into_owned();

    let first = {
        let system = build_system(&root, Vec::new());
        system.new_session(SessionPersistence::Persistent).unwrap()
    };
    assert!(DiskFs::exists(&format!("{root}/session_manager.bin")));
    assert!(DiskFs::exists(&format!("{root}/tool_registry.bin")));
    assert!(DiskFs::exists(&format!(
        "{root}/sessions/{}.bin",
        first.to_wire()
    )));

    let system = build_system(&root, Vec::new());
    assert_eq!(system.list_sessions(), vec![first]);
    let second = system.new_session(SessionPersistence::Persistent).unwrap();
    assert_eq!(second, SessionId::new(first.0.saturating_add(1)));
    assert_eq!(system.list_sessions(), vec![first, second]);
}

#[test]
fn session_manager_payload_contains_both_counters() {
    let _script = serialize_script();
    let temp = TempDir::new("runtime-payload").unwrap();
    let root = temp.path().to_string_lossy().into_owned();
    {
        let system = build_system(&root, Vec::new());
        let _ = system.new_session(SessionPersistence::Persistent).unwrap();
    }

    let state = read_payload(&format!("{root}/session_manager.bin"));
    assert_eq!(
        state,
        json!({
            "agent_ids": "agent-1",
            "session_ids": "session-2",
        })
    );
}

#[test]
fn root_inflight_toolcall_is_on_disk_before_the_tool_body_can_finish() {
    let _script = serialize_script();
    let temp = TempDir::new("inflight-toolcall").unwrap();
    let root = temp.path().to_string_lossy().into_owned();
    install_script(vec![
        assistant_tool_call("blocking_tool", json!({ "value": "held" })),
        support::assistant_text("done"),
    ]);

    let gate = Arc::new(ToolGate::default());
    let system = build_configured_system(&root);
    system
        .tool_registry()
        .register_group(ToolGroup::new(
            "test",
            true,
            [Tool::from_async(BlockingTool {
                gate: Arc::clone(&gate),
            })],
        ))
        .unwrap();
    system.start_all().unwrap();
    let session = system.new_session(SessionPersistence::Persistent).unwrap();
    let mut events = system.open_session(session).unwrap();
    let control = events.control();
    block_on(control.append(Message::text("run the blocking tool"))).unwrap();

    wait_until(&gate.started, "blocking tool did not start");
    let path = format!("{root}/sessions/{}.bin", session.to_wire());
    let inflight = read_payload(&path);
    assert_eq!(
        inflight["root_inflight_toolcalls"],
        json!([{
            "id": "call_persistence_1",
            "name": "blocking_tool",
            "arguments_json": r#"{"value":"held"}"#
        }])
    );

    gate.release();
    let _ = support::drain_until_turn_ended(&mut events);
    drop(control);
    drop(events);
    drop(system);

    let completed = read_payload(&path);
    assert_eq!(completed["root_inflight_toolcalls"], json!([]));
}

#[test]
fn deleting_an_inflight_session_removes_its_root_agent_and_transcript() {
    let _script = serialize_script();
    let temp = TempDir::new("delete-inflight-session").unwrap();
    let root = temp.path().to_string_lossy().into_owned();
    install_script(vec![
        assistant_tool_call("blocking_tool", json!({ "value": "held" })),
        support::assistant_text("done"),
    ]);

    let gate = Arc::new(ToolGate::default());
    let system = Arc::new(build_configured_system(&root));
    system
        .tool_registry()
        .register_group(ToolGroup::new(
            "test",
            true,
            [Tool::from_async(BlockingTool {
                gate: Arc::clone(&gate),
            })],
        ))
        .unwrap();
    system.start_all().unwrap();
    let session = system.new_session(SessionPersistence::Persistent).unwrap();
    let mut events = system.open_session(session).unwrap();
    let control = events.control();
    block_on(control.append(Message::text("run the blocking tool"))).unwrap();

    wait_until(&gate.started, "blocking tool did not start");
    let agent_state = format!("{root}/agents/agent-1.bin");
    let transcript_data = format!("{root}/transcript/1.jsonl");
    let transcript_index = format!("{root}/transcript/1.json");
    assert!(DiskFs::exists(&agent_state));

    let deleting = Arc::clone(&system);
    let delete = thread::spawn(move || deleting.delete_session(session));
    gate.release();
    delete.join().unwrap().unwrap();

    assert!(!DiskFs::exists(&agent_state));
    assert!(!DiskFs::exists(&transcript_data));
    assert!(!DiskFs::exists(&transcript_index));
    assert!(!DiskFs::exists(&format!(
        "{root}/sessions/{}.bin",
        session.to_wire()
    )));
    block_on(async {
        while let Some(event) = events.next().await {
            if event == claw_agent::SessionEvent::Closed {
                return;
            }
        }
        panic!("deleted Session stream ended without Closed");
    });
}

#[test]
fn session_payload_contains_only_restart_relevant_state() {
    let _script = serialize_script();
    let temp = TempDir::new("session-payload").unwrap();
    let root = temp.path().to_string_lossy().into_owned();
    let session = {
        let system = build_system(&root, Vec::new());
        system.new_session(SessionPersistence::Persistent).unwrap()
    };

    let json = read_payload(&format!("{root}/sessions/{}.bin", session.to_wire()));
    assert_eq!(
        json,
        json!({
            "reasoning_effort": "medium",
            "permission_level": "allow_all",
            "root_agent": null,
            "root_inflight_toolcalls": [],
        })
    );
}

#[test]
fn startup_purges_agent_and_transcript_without_a_referencing_session() {
    let _script = serialize_script();
    let temp = TempDir::new("purge-orphan-agent").unwrap();
    let root = temp.path().to_string_lossy().into_owned();
    let session = create_persisted_root(&root);
    let session_state = format!("{root}/sessions/{}.bin", session.to_wire());
    let agent_state = format!("{root}/agents/agent-1.bin");
    let transcript_data = format!("{root}/transcript/1.jsonl");
    let transcript_index = format!("{root}/transcript/1.json");

    DiskFs::remove(&session_state).unwrap();
    assert!(DiskFs::exists(&agent_state));
    assert!(DiskFs::exists(&transcript_data));
    assert!(DiskFs::exists(&transcript_index));

    drop(build_system(&root, Vec::new()));

    assert!(!DiskFs::exists(&agent_state));
    assert!(!DiskFs::exists(&transcript_data));
    assert!(!DiskFs::exists(&transcript_index));
}

#[test]
fn startup_purges_transcript_without_an_agent_record() {
    let _script = serialize_script();
    let temp = TempDir::new("purge-orphan-transcript").unwrap();
    let root = temp.path().to_string_lossy().into_owned();
    let transcript_data = format!("{root}/transcript/41.jsonl");
    let transcript_index = format!("{root}/transcript/41.json");

    DiskFs::write_atomic(&transcript_data, b"").unwrap();
    DiskFs::write_atomic(&transcript_index, b"").unwrap();
    assert!(DiskFs::exists(&transcript_data));
    assert!(DiskFs::exists(&transcript_index));

    drop(build_system(&root, Vec::new()));

    assert!(!DiskFs::exists(&transcript_data));
    assert!(!DiskFs::exists(&transcript_index));
}

#[test]
fn startup_clears_a_session_root_whose_agent_record_is_missing() {
    let _script = serialize_script();
    let temp = TempDir::new("purge-dangling-root").unwrap();
    let root = temp.path().to_string_lossy().into_owned();
    let session = create_persisted_root(&root);
    let session_state = format!("{root}/sessions/{}.bin", session.to_wire());
    let agent_state = format!("{root}/agents/agent-1.bin");
    let transcript_data = format!("{root}/transcript/1.jsonl");
    let transcript_index = format!("{root}/transcript/1.json");

    DiskFs::remove(&agent_state).unwrap();
    assert!(DiskFs::exists(&transcript_data));
    assert!(DiskFs::exists(&transcript_index));

    drop(build_system(&root, Vec::new()));

    let state = read_payload(&session_state);
    assert_eq!(state["root_agent"], Value::Null);
    assert_eq!(state["root_inflight_toolcalls"], json!([]));
    assert!(!DiskFs::exists(&transcript_data));
    assert!(!DiskFs::exists(&transcript_index));
}

#[test]
fn ephemeral_sessions_do_not_enter_the_persisted_collection() {
    let _script = serialize_script();
    let temp = TempDir::new("ephemeral-session").unwrap();
    let root = temp.path().to_string_lossy().into_owned();

    let ephemeral = {
        let system = build_system(&root, Vec::new());
        let session = system.new_session(SessionPersistence::Ephemeral).unwrap();
        assert_eq!(system.list_sessions(), vec![session]);
        assert!(!DiskFs::exists(&format!(
            "{root}/sessions/{}.bin",
            session.to_wire()
        )));
        session
    };

    let system = build_system(&root, Vec::new());
    assert!(system.list_sessions().is_empty());
    assert_eq!(
        system.new_session(SessionPersistence::Persistent).unwrap(),
        SessionId::new(ephemeral.0.saturating_add(1))
    );
}

#[test]
fn explicit_tool_override_survives_rebuild() {
    let _script = serialize_script();
    let temp = TempDir::new("tool-overrides").unwrap();
    let root = temp.path().to_string_lossy().into_owned();

    {
        let system = build_system(&root, Vec::new());
        system
            .tool_registry()
            .register_group(ToolGroup::new("test", true, [Tool::from_sync(EchoTool)]))
            .unwrap();
        system.tool_registry().disable("echo").unwrap();
    }

    let system = build_system(&root, Vec::new());
    system
        .tool_registry()
        .register_group(ToolGroup::new("test", true, [Tool::from_sync(EchoTool)]))
        .unwrap();
    system.start_all().unwrap();

    let json = read_payload(&format!("{root}/tool_registry.bin"));
    assert_eq!(json["overrides"]["echo"], false);
}

#[test]
fn deleting_a_session_removes_it_from_the_directory_registry() {
    let _script = serialize_script();
    let temp = TempDir::new("delete-session").unwrap();
    let root = temp.path().to_string_lossy().into_owned();
    let session = {
        let system = build_system(&root, Vec::new());
        let session = system.new_session(SessionPersistence::Persistent).unwrap();
        system.delete_session(session).unwrap();
        assert!(!DiskFs::exists(&format!(
            "{root}/sessions/{}.bin",
            session.to_wire()
        )));
        session
    };

    let system = build_system(&root, Vec::new());
    assert!(!system.list_sessions().contains(&session));
}

fn create_persisted_root(root: &str) -> SessionId {
    let system = build_system(root, vec![support::assistant_text("done")]);
    system.start_all().unwrap();
    let session = system.new_session(SessionPersistence::Persistent).unwrap();
    let mut events = system.open_session(session).unwrap();
    block_on(events.append(Message::text("create the root agent"))).unwrap();
    let _ = support::drain_until_turn_ended(&mut events);
    drop(events);
    drop(system);
    session
}

fn read_payload(path: &str) -> Value {
    let bytes = DiskFs::read(path).unwrap();
    assert!(bytes.len() >= std::mem::size_of::<u32>());
    serde_json::from_slice(&bytes[std::mem::size_of::<u32>()..]).unwrap()
}

fn build_system(root: &str, bodies: Vec<String>) -> DiskSystem {
    install_script(bodies);
    build_configured_system(root)
}

fn build_configured_system(root: &str) -> DiskSystem {
    let system = DiskSystem::new::<StdThread, TokioExecutor>(AgentPersistenceConfig {
        persistence_root: root.to_owned(),
        skill_roots: Vec::new(),
    })
    .unwrap();
    system
        .link_api(llm_config(), claw_agent::ApiUsage::RootAgent, true)
        .unwrap();
    system
}

fn assistant_tool_call(name: &str, arguments: Value) -> String {
    json!({
        "choices": [{
            "message": {
                "role": "assistant",
                "tool_calls": [{
                    "id": "call_persistence_1",
                    "type": "function",
                    "function": {
                        "name": name,
                        "arguments": arguments.to_string()
                    }
                }]
            }
        }]
    })
    .to_string()
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

struct BlockingTool {
    gate: Arc<ToolGate>,
}

impl ToolSpec for BlockingTool {
    fn name(&self) -> &str {
        "blocking_tool"
    }

    fn schema(&self) -> &str {
        r#"{"type":"function","function":{"name":"blocking_tool"}}"#
    }
}

impl AsyncToolHandler for BlockingTool {
    fn invoke<'a>(&'a self, _call: &'a ToolInvocation<'_>) -> ToolFuture<'a> {
        Box::pin(poll_fn(move |context| {
            self.gate.started.store(true, Ordering::SeqCst);
            if self.gate.released.load(Ordering::SeqCst) {
                return Poll::Ready(Ok(ToolOutput {
                    output: "released".to_owned(),
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

struct EchoTool;

impl ToolSpec for EchoTool {
    fn name(&self) -> &str {
        "echo"
    }

    fn schema(&self) -> &str {
        r#"{"type":"function","function":{"name":"echo"}}"#
    }
}

impl SyncToolHandler for EchoTool {
    fn invoke(&self, call: &ToolInvocation<'_>) -> ToolResult<ToolOutput> {
        Ok(ToolOutput {
            output: call.arguments_json().to_owned(),
            ok: true,
        })
    }
}
