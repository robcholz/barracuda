#![allow(clippy::unwrap_used)]

mod support;

use claw_agent::{AgentPersistenceConfig, AgentSystem, SessionPersistence};
use claw_interface::{
    BlockingHttpAdapter, ClawFs, DiskFs, ImmediateTimer, SharedScriptHttp, StdThread, TokioExecutor,
};
use tempdir::TempDir;

use support::Sse;

type DiskSystem = AgentSystem<DiskFs, Sse<BlockingHttpAdapter<SharedScriptHttp>>, ImmediateTimer>;

#[test]
fn short_id_allocator_header_rejects_startup() {
    let root = TempDir::new("claw-persistence-short-header").unwrap();
    DiskFs::write_atomic(
        &format!("{}/id_allocators.bin", root.path().display()),
        b"x",
    )
    .unwrap();

    assert_startup_error(root.path().to_str().unwrap(), "too short");
}

#[test]
fn invalid_id_allocator_json_rejects_startup() {
    let root = TempDir::new("claw-persistence-invalid-json").unwrap();
    write_state(
        &format!("{}/id_allocators.bin", root.path().display()),
        1,
        b"{bad-json",
    );

    assert_startup_error(root.path().to_str().unwrap(), "decode durable state");
}

#[test]
fn unsupported_id_allocator_schema_rejects_startup() {
    let root = TempDir::new("claw-persistence-id-allocator-schema").unwrap();
    write_state(
        &format!("{}/id_allocators.bin", root.path().display()),
        99,
        br#"{"next_session_id":1,"next_agent_id":1}"#,
    );

    assert_startup_error(
        root.path().to_str().unwrap(),
        "unsupported id allocator state schema",
    );
}

#[test]
fn unsupported_tool_registry_schema_rejects_startup() {
    let root = TempDir::new("claw-persistence-tool-registry-schema").unwrap();
    write_state(
        &format!("{}/tool_registry.bin", root.path().display()),
        99,
        br#"{"overrides":{}}"#,
    );

    assert_startup_error(
        root.path().to_str().unwrap(),
        "unsupported tool registry state schema",
    );
}

#[test]
fn unsupported_session_schema_rejects_rebuild() {
    let root = TempDir::new("claw-persistence-session-schema").unwrap();
    let root_path = root.path().to_str().unwrap();
    let session = {
        let system = build(root_path).unwrap();
        system.new_session(SessionPersistence::Persistent).unwrap()
    };
    write_state(
        &format!("{root_path}/sessions/{}.bin", session.to_wire()),
        99,
        b"{}",
    );

    assert_startup_error(root_path, "unsupported session state schema");
}

fn build(root: &str) -> Result<DiskSystem, claw_agent::AgentError> {
    DiskSystem::new::<StdThread, TokioExecutor>(AgentPersistenceConfig {
        persistence_root: root.to_owned(),
        skill_roots: Vec::new(),
    })
}

fn assert_startup_error(root: &str, expected: &str) {
    let error = match build(root) {
        Ok(_) => panic!("corrupt state must reject startup"),
        Err(error) => error,
    };
    let chain = std::iter::successors(
        Some(&error as &(dyn std::error::Error + 'static)),
        |error| error.source(),
    )
    .map(ToString::to_string)
    .collect::<Vec<_>>();
    assert!(
        chain.iter().any(|message| message.contains(expected)),
        "expected `{expected}` in error chain {chain:?}"
    );
}

fn write_state(path: &str, schema_version: u32, payload: &[u8]) {
    let mut bytes = schema_version.to_le_bytes().to_vec();
    bytes.extend_from_slice(payload);
    DiskFs::write_atomic(path, &bytes).unwrap();
}
