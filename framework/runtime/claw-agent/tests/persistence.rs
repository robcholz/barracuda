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
    assert!(DiskFs::exists(&format!("{root}/session_id_allocator.bin")));
    assert!(DiskFs::exists(&format!("{root}/agent_id_allocator.bin")));
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
fn id_allocator_payloads_contain_only_their_owned_counters() {
    let _script = serialize_script();
    let temp = TempDir::new("runtime-payload").unwrap();
    let root = temp.path().to_string_lossy().into_owned();
    {
        let system = build_system(&root, Vec::new());
        let _ = system.new_session(SessionPersistence::Persistent).unwrap();
    }

    let session_ids = read_payload(&format!("{root}/session_id_allocator.bin"));
    assert_eq!(session_ids, json!({ "next_session_id": 2 }));

    let agent_ids = read_payload(&format!("{root}/agent_id_allocator.bin"));
    assert_eq!(agent_ids, json!({ "next_agent_id": 1 }));
}

#[test]
fn inflight_toolcall_is_on_disk_before_the_tool_body_can_finish() {
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
    let (control, mut events) = system.open_session(session).unwrap();
    block_on(control.submit(Message::text("run the blocking tool"))).unwrap();

    wait_until(&gate.started, "blocking tool did not start");
    let path = format!("{root}/sessions/{}.bin", session.to_wire());
    let inflight = read_payload(&path);
    assert_eq!(
        inflight["resume"]["inflight_toolcalls"],
        json!([{
            "tool": "blocking_tool",
            "arguments": { "value": "held" }
        }])
    );

    gate.release();
    let _ = support::drain_until_turn_ended(&mut events);
    drop(control);
    drop(events);
    drop(system);

    let completed = read_payload(&path);
    assert!(
        completed.get("resume").is_none(),
        "a completed transcript turn settles its persisted tool calls: {completed}"
    );
}

#[test]
fn legacy_combined_id_allocator_dto_is_split_during_construction() {
    let _script = serialize_script();
    let temp = TempDir::new("legacy-id-allocator-checkpoint").unwrap();
    let root = temp.path().to_string_lossy().into_owned();
    write_payload(
        &format!("{root}/id_allocators.bin"),
        &json!({ "next_session_id": 4, "next_agent_id": 7 }),
    );

    {
        let system = build_system(&root, Vec::new());
        assert_eq!(
            system.new_session(SessionPersistence::Persistent).unwrap(),
            SessionId::new(4)
        );
    }

    assert_eq!(
        read_payload(&format!("{root}/session_id_allocator.bin")),
        json!({ "next_session_id": 5 })
    );
    assert_eq!(
        read_payload(&format!("{root}/agent_id_allocator.bin")),
        json!({ "next_agent_id": 7 })
    );
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
    assert_eq!(json["reasoning_effort"], "medium");
    assert_eq!(json["permission_level"], "allow_all");
    assert_eq!(json["mode"], "normal");
    assert!(json.get("resume").is_none());
    assert!(json.get("active_turn").is_none());
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

#[test]
fn unopened_session_does_not_consume_its_recovery_journal() {
    let _script = serialize_script();
    let temp = TempDir::new("unopened-session-recovery").unwrap();
    let root = temp.path().to_string_lossy().into_owned();
    let session = {
        let system = build_system(&root, Vec::new());
        system.new_session(SessionPersistence::Persistent).unwrap()
    };
    let path = format!("{root}/sessions/{}.bin", session.to_wire());
    let mut payload = read_payload(&path);
    payload["resume"] = json!({
        "tool_set": { "loaded_groups": ["profile"] },
        "inflight_toolcalls": []
    });
    write_payload(&path, &payload);

    {
        let system = build_system(&root, Vec::new());
        assert_eq!(system.list_sessions(), vec![session]);
    }

    assert_eq!(
        read_payload(&path)["resume"]["tool_set"]["loaded_groups"],
        json!(["profile"])
    );
}

#[test]
fn opening_without_a_turn_does_not_consume_the_recovery_reminder() {
    let _script = serialize_script();
    let temp = TempDir::new("idle-open-session-recovery").unwrap();
    let root = temp.path().to_string_lossy().into_owned();
    let session = {
        let system = build_system(&root, Vec::new());
        system.new_session(SessionPersistence::Persistent).unwrap()
    };
    let path = format!("{root}/sessions/{}.bin", session.to_wire());
    let mut payload = read_payload(&path);
    payload["resume"] = json!({
        "tool_set": { "loaded_groups": ["profile"] },
        "inflight_toolcalls": []
    });
    write_payload(&path, &payload);

    {
        let system = build_system(&root, Vec::new());
        let (control, _events) = system.open_session(session).unwrap();
        futures_lite::future::block_on(control.close_session()).unwrap();
    }

    assert_eq!(
        read_payload(&path)["resume"]["tool_set"]["loaded_groups"],
        json!(["profile"])
    );
}

fn read_payload(path: &str) -> Value {
    let bytes = DiskFs::read(path).unwrap();
    assert!(bytes.len() >= std::mem::size_of::<u32>());
    serde_json::from_slice(&bytes[std::mem::size_of::<u32>()..]).unwrap()
}

fn write_payload(path: &str, payload: &Value) {
    let mut bytes = 1u32.to_le_bytes().to_vec();
    bytes.extend(serde_json::to_vec(payload).unwrap());
    DiskFs::write_atomic(path, &bytes).unwrap();
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
