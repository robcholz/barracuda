#![allow(clippy::unwrap_used)]

mod support;
use support::Sse;

use std::collections::BTreeMap;

use claw_agent::{
    stream::StreamPart, AgentError, AgentSystem, IterationEvent, IterationId, Message,
    OpenSessionError, SessionControlError, SessionEvent, SessionId, SessionStream, TurnEvent,
    TurnId, TurnOrigin,
};
use claw_interface::{
    BlockingHttpAdapter, ClawFs, DiskFs, ImmediateTimer, SharedScriptHttp, StdThread, TokioExecutor,
};
use claw_tool::{
    SyncToolHandler, Tool, ToolGroup, ToolInvocation, ToolOutput, ToolResult, ToolSpec,
};
use futures_lite::future::block_on;
use futures_lite::StreamExt;
use support::{
    assistant_text, build_mem_system, csv_dicts, drain_until_turn_ended, install_script,
    llm_config, mem_root, persistence, serialize_script, try_build_mem_system_with_tool_groups,
};
use tempdir::TempDir;

type DiskAgentSystem =
    AgentSystem<DiskFs, Sse<BlockingHttpAdapter<SharedScriptHttp>>, ImmediateTimer>;

#[test]
fn submit_streams_csv_reply_cases() {
    let _script = serialize_script();

    for row in csv_dicts(include_str!("fixtures/session_submit_cases.csv")) {
        let case = field(&row, "case");
        let root = mem_root("csv-submit");
        let expected_output = field(&row, "assistant_output");
        let bodies = if expected_output.is_empty() {
            Vec::new()
        } else {
            vec![assistant_text(expected_output)]
        };
        let system = build_mem_system(&root, bodies);
        let session = system
            .new_session(claw_agent::SessionPersistence::Persistent)
            .unwrap();
        let (control, mut events) = system.open_session(session).unwrap();

        block_on(control.append(Message::text(field(&row, "user_input")))).unwrap();
        let events = drain_until_turn_ended(&mut events);

        assert!(
            matches!(
                events.first(),
                Some(SessionEvent::Turn(TurnEvent::Started {
                    turn: TurnId(1),
                    origin: TurnOrigin::User,
                }))
            ),
            "case {case}"
        );
        assert!(
            matches!(
                events.last(),
                Some(SessionEvent::Turn(TurnEvent::Ended { turn: TurnId(1) }))
            ),
            "case {case}"
        );
        assert_eq!(
            output_fragments(&events),
            expected_output_fragments(expected_output),
            "case {case}"
        );
    }
}

#[test]
fn prefixed_ids_round_trip_csv_wire_cases() {
    for row in csv_dicts(include_str!("fixtures/id_wire_valid_cases.csv")) {
        let raw = field(&row, "raw").parse::<u32>().unwrap();
        let wire = field(&row, "wire");

        match field(&row, "kind") {
            "session" => {
                let id = SessionId::new(raw);
                assert_eq!(id.to_wire(), wire);
                assert_eq!(id.to_string(), wire);
                assert_eq!(SessionId::from_wire(wire).unwrap(), id);
                assert_eq!(wire.parse::<SessionId>().unwrap(), id);
            }
            "turn" => {
                let id = TurnId::new(raw);
                assert_eq!(id.to_wire(), wire);
                assert_eq!(id.to_string(), wire);
                assert_eq!(TurnId::from_wire(wire).unwrap(), id);
                assert_eq!(wire.parse::<TurnId>().unwrap(), id);
            }
            "iteration" => {
                let id = IterationId::new(raw);
                assert_eq!(id.to_wire(), wire);
                assert_eq!(id.to_string(), wire);
                assert_eq!(IterationId::from_wire(wire).unwrap(), id);
                assert_eq!(wire.parse::<IterationId>().unwrap(), id);
            }
            other => panic!("unknown id kind in fixture: {other}"),
        }
    }
}

#[test]
fn prefixed_ids_reject_csv_invalid_wire_cases() {
    for row in csv_dicts(include_str!("fixtures/id_wire_invalid_cases.csv")) {
        let input = field(&row, "input");
        let actual = match field(&row, "kind") {
            "session" => SessionId::from_wire(input).unwrap_err().to_string(),
            "turn" => TurnId::from_wire(input).unwrap_err().to_string(),
            "iteration" => IterationId::from_wire(input).unwrap_err().to_string(),
            other => panic!("unknown id kind in fixture: {other}"),
        };

        assert_eq!(actual, field(&row, "error"), "input {input:?}");
    }
}

#[test]
fn tool_configuration_csv_cases_report_public_errors() {
    let _script = serialize_script();

    for row in csv_dicts(include_str!("fixtures/tool_registry_cases.csv")) {
        let case = field(&row, "case");
        let root = mem_root("csv-tools");
        let operations = field(&row, "operations");
        let actual_error = match try_build_mem_system_with_tool_groups(
            &root,
            Vec::new(),
            tool_groups_from_operations(operations),
        ) {
            Ok(system) => apply_tool_operations(&system, operations),
            Err(error) => Some(error.to_string()),
        };
        assert_expected_error(actual_error.as_deref(), field(&row, "error_contains"), case);
    }
}

#[test]
fn session_lifecycle_csv_cases_return_precise_public_results() {
    let _script = serialize_script();

    for row in csv_dicts(include_str!("fixtures/session_lifecycle_cases.csv")) {
        let case = field(&row, "case");
        let root = mem_root("csv-lifecycle");
        let system = build_mem_system(&root, Vec::new());
        let actual_error = match field(&row, "operation") {
            "open_twice" => lifecycle_open_twice(&system),
            "delete_unknown" => lifecycle_delete_unknown(&system),
            "reopen_after_close" => lifecycle_reopen_after_close(&system),
            "interrupt_after_close" => {
                lifecycle_control_after_close(&system, ControlAfterClose::Interrupt)
            }
            "cancel_after_close" => {
                lifecycle_control_after_close(&system, ControlAfterClose::Cancel)
            }
            "delete_after_close" => lifecycle_delete_after_close(&system),
            other => panic!("unknown lifecycle operation in fixture: {other}"),
        };

        assert_expected_error(actual_error.as_deref(), field(&row, "expected_error"), case);
    }
}

#[test]
fn construction_csv_roots_accept_tempdirs_and_reject_blank_roots() {
    let _script = serialize_script();

    for row in csv_dicts(include_str!("fixtures/construction_roots.csv")) {
        let case = field(&row, "case");
        let expect_ok = parse_bool(field(&row, "expect_ok"));

        if expect_ok {
            let temp = TempDir::new("claw-agent-api-root").unwrap();
            let root = disk_root(field(&row, "root_mode"), &temp);
            let system = match try_build_disk_system(&root) {
                Ok(system) => system,
                Err(error) => panic!("case {case} should build: {error}"),
            };
            let session = system
                .new_session(claw_agent::SessionPersistence::Persistent)
                .unwrap();

            assert_eq!(system.list_sessions(), vec![session], "case {case}");
            drop(system);
            assert!(
                DiskFs::exists(&format!(
                    "{}/session_manager.bin",
                    root.trim_end_matches('/')
                )),
                "case {case}"
            );
        } else {
            let root = invalid_root(field(&row, "root_mode"));
            match try_build_mem_system(&root) {
                Ok(_) => panic!("case {case} should reject root {root:?}"),
                Err(error) => assert!(
                    error.to_string().contains(field(&row, "error_contains")),
                    "case {case}: {error}"
                ),
            }
        }
    }
}

struct CsvTool {
    name: String,
    schema: String,
}

impl CsvTool {
    fn new(name: &str) -> Self {
        Self {
            name: name.to_owned(),
            schema: format!(r#"{{"type":"function","function":{{"name":"{name}"}}}}"#),
        }
    }
}

impl ToolSpec for CsvTool {
    fn name(&self) -> &str {
        &self.name
    }

    fn schema(&self) -> &str {
        &self.schema
    }
}

impl SyncToolHandler for CsvTool {
    fn invoke(&self, call: &ToolInvocation) -> ToolResult<ToolOutput> {
        Ok(ToolOutput {
            content: call.arguments_json().to_owned(),
            ok: true,
        })
    }
}

enum ControlAfterClose {
    Interrupt,
    Cancel,
}

fn lifecycle_open_twice(system: &support::MemAgentSystem) -> Option<String> {
    let session = system
        .new_session(claw_agent::SessionPersistence::Persistent)
        .unwrap();
    let (_control, _events) = system.open_session(session).unwrap();
    match system.open_session(session) {
        Ok(_) => panic!("second open should fail"),
        Err(AgentError::OpenSession(OpenSessionError::AlreadyOpen(open))) if open == session => {
            Some(format!("session is already open: {open}"))
        }
        Err(error) => Some(error.to_string()),
    }
}

fn lifecycle_delete_unknown(system: &support::MemAgentSystem) -> Option<String> {
    match system.delete_session(SessionId(404)) {
        Ok(()) => None,
        Err(error) => Some(error.to_string()),
    }
}

fn lifecycle_reopen_after_close(system: &support::MemAgentSystem) -> Option<String> {
    let session = system
        .new_session(claw_agent::SessionPersistence::Persistent)
        .unwrap();
    let (control, mut events) = system.open_session(session).unwrap();
    block_on(control.close()).unwrap();
    assert_closed(&mut events);

    let (reopened_control, mut reopened_events) = system.open_session(session).unwrap();
    block_on(reopened_control.close()).unwrap();
    assert_closed(&mut reopened_events);
    None
}

fn lifecycle_control_after_close(
    system: &support::MemAgentSystem,
    control_kind: ControlAfterClose,
) -> Option<String> {
    let session = system
        .new_session(claw_agent::SessionPersistence::Persistent)
        .unwrap();
    let (control, mut events) = system.open_session(session).unwrap();
    block_on(control.close()).unwrap();
    assert_closed(&mut events);

    let result = block_on(async {
        match control_kind {
            ControlAfterClose::Interrupt => control.interrupt().await,
            ControlAfterClose::Cancel => control.cancel().await,
        }
    });
    match result {
        Ok(()) => None,
        Err(SessionControlError::SessionClosed(closed)) if closed == session => {
            Some(format!("session is closed: {closed}"))
        }
        Err(error) => Some(error.to_string()),
    }
}

fn lifecycle_delete_after_close(system: &support::MemAgentSystem) -> Option<String> {
    let session = system
        .new_session(claw_agent::SessionPersistence::Persistent)
        .unwrap();
    let (control, mut events) = system.open_session(session).unwrap();
    block_on(control.close()).unwrap();
    assert_closed(&mut events);

    match system.delete_session(session) {
        Ok(()) => {
            assert!(!system.list_sessions().contains(&session));
            None
        }
        Err(error) => Some(error.to_string()),
    }
}

fn assert_closed(events: &mut SessionStream) {
    let events = drain_until_closed(events);
    assert!(matches!(events.last(), Some(SessionEvent::Closed(_))));
}

fn drain_until_closed(events: &mut SessionStream) -> Vec<SessionEvent> {
    block_on(async move {
        let mut collected = Vec::new();
        while let Some(event) = events.next().await {
            let event = event.expect("Session stream failed");
            let closed = matches!(event, SessionEvent::Closed(_));
            collected.push(event);
            if closed {
                break;
            }
        }
        collected
    })
}

fn apply_tool_operations(system: &support::MemAgentSystem, operations: &str) -> Option<String> {
    for operation in operations.split('|') {
        let result = match operation {
            "start" => system.start_all().map_err(|error| error.to_string()),
            "stop" => system.stop_all().map_err(|error| error.to_string()),
            _ if operation.starts_with("register:") => Ok(()),
            _ if operation.starts_with("enable:") => {
                let name = &operation["enable:".len()..];
                system.enable_tool(name).map_err(|error| error.to_string())
            }
            _ if operation.starts_with("disable:") => {
                let name = &operation["disable:".len()..];
                system.disable_tool(name).map_err(|error| error.to_string())
            }
            other => panic!("unknown tool operation in fixture: {other}"),
        };

        if let Err(error) = result {
            return Some(error);
        }
    }
    None
}

fn tool_groups_from_operations(operations: &str) -> Vec<ToolGroup> {
    operations
        .split('|')
        .filter_map(|operation| operation.strip_prefix("register:").map(str::to_owned))
        .enumerate()
        .map(|(index, name)| {
            ToolGroup::new(
                format!("csv-group-{}", index.saturating_add(1)),
                true,
                [Tool::from_sync(CsvTool::new(&name))],
            )
        })
        .collect()
}

fn assert_expected_error(actual: Option<&str>, expected_contains: &str, case: &str) {
    if expected_contains.is_empty() {
        assert!(actual.is_none(), "case {case}: unexpected error {actual:?}");
    } else {
        let actual = actual.unwrap_or_else(|| panic!("case {case}: expected an error"));
        assert!(
            actual.contains(expected_contains),
            "case {case}: expected {actual:?} to contain {expected_contains:?}"
        );
    }
}

fn output_fragments(events: &[SessionEvent]) -> Vec<String> {
    events
        .iter()
        .filter_map(|event| match event {
            SessionEvent::Turn(
                TurnEvent::Output(StreamPart::Delta(text))
                | TurnEvent::Iteration(IterationEvent::Output(StreamPart::Delta(text))),
            ) => Some(text.clone()),
            _ => None,
        })
        .collect()
}

fn expected_output_fragments(expected_output: &str) -> Vec<String> {
    if expected_output.is_empty() {
        Vec::new()
    } else {
        vec![expected_output.to_owned()]
    }
}

fn try_build_mem_system(root: &str) -> Result<support::MemAgentSystem, AgentError> {
    install_script(Vec::<String>::new());
    let system = support::MemAgentSystem::new::<StdThread, TokioExecutor>(persistence(root))?;
    system
        .link_api(llm_config(), claw_agent::ApiUsage::RootAgent, true)
        .unwrap();
    Ok(system)
}

fn try_build_disk_system(root: &str) -> Result<DiskAgentSystem, AgentError> {
    install_script(Vec::<String>::new());
    let system = DiskAgentSystem::new::<StdThread, TokioExecutor>(persistence(root))?;
    system
        .link_api(llm_config(), claw_agent::ApiUsage::RootAgent, true)
        .unwrap();
    Ok(system)
}

fn disk_root(mode: &str, temp: &TempDir) -> String {
    let root = temp.path().to_string_lossy();
    match mode {
        "plain" => root.into_owned(),
        "trailing" => format!("{root}/"),
        "nested" => temp
            .path()
            .join("nested")
            .join("root")
            .to_string_lossy()
            .into_owned(),
        other => panic!("unsupported disk root mode: {other}"),
    }
}

fn invalid_root(mode: &str) -> String {
    match mode {
        "empty" => String::new(),
        "blank" => "   ".to_string(),
        other => panic!("unsupported invalid root mode: {other}"),
    }
}

fn parse_bool(value: &str) -> bool {
    match value {
        "true" => true,
        "false" => false,
        other => panic!("invalid bool in fixture: {other}"),
    }
}

fn field<'a>(row: &'a BTreeMap<String, String>, name: &str) -> &'a str {
    row.get(name)
        .unwrap_or_else(|| panic!("missing csv column {name}"))
        .as_str()
}
