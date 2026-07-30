#![allow(clippy::unwrap_used)]

mod support;

use std::sync::{Arc, Mutex, MutexGuard};

use claw_agent::tools::{
    SyncToolHandler, Tool, ToolGroup, ToolInvocation, ToolOutput, ToolResult, ToolSpec,
};
use claw_agent::Message;
use claw_log::{FlatTreeSubscriber, TraceSink};
use futures_lite::future::block_on;
use serde_json::json;
use tracing::Level;

use support::{
    assistant_text, drain_until_turn_ended, mem_root, serialize_script,
    try_build_mem_system_with_tool_groups,
};

const ARGUMENT_PAYLOAD_SECRET: &str = "tool-argument-must-not-appear-in-trace-3c72";
const TOOL_OUTPUT_SECRET: &str = "tool-output-must-not-appear-in-trace-91ab";

#[derive(Clone, Default)]
struct RecordingSink(Arc<Mutex<Vec<String>>>);

impl RecordingSink {
    fn lines(&self) -> Vec<String> {
        lock(&self.0).clone()
    }
}

impl TraceSink for RecordingSink {
    fn write_line(&self, _level: Level, _tag: &str, line: &str) {
        lock(&self.0).push(line.to_string());
    }
}

struct TraceEcho;

impl ToolSpec for TraceEcho {
    fn name(&self) -> &str {
        "trace_echo"
    }

    fn schema(&self) -> &str {
        r#"{"type":"function","function":{"name":"trace_echo","parameters":{"type":"object"}}}"#
    }
}

impl SyncToolHandler for TraceEcho {
    fn invoke(&self, _call: &ToolInvocation) -> ToolResult<ToolOutput> {
        Ok(ToolOutput {
            content: TOOL_OUTPUT_SECRET.to_owned(),
            ok: true,
        })
    }
}

#[test]
fn toolcalls_follow_the_runtime_trace_contract_without_payloads() {
    let sink = RecordingSink::default();
    let subscriber = FlatTreeSubscriber::with_sink(sink.clone())
        .with_allowed_target_prefix("claw")
        .with_context_group_keys("run", ["system", "session", "turn", "agent", "iteration"]);
    tracing::subscriber::set_global_default(subscriber)
        .expect("this single-test binary installs tracing exactly once");

    let _script = serialize_script();
    let arguments = json!({ "secret": ARGUMENT_PAYLOAD_SECRET }).to_string();
    let system = try_build_mem_system_with_tool_groups(
        &mem_root("tool-runtime-trace"),
        vec![
            assistant_tool_call(&arguments),
            assistant_text("tool round finished"),
        ],
        [ToolGroup::new("trace", true, [Tool::from_sync(TraceEcho)])],
    )
    .unwrap();
    system.start_all().unwrap();
    let session = system
        .new_session(claw_agent::SessionPersistence::Ephemeral)
        .unwrap();
    let (control, mut events) = system.open_session(session).unwrap();

    block_on(control.append(Message::text("exercise tool tracing"))).unwrap();
    let _ = drain_until_turn_ended(&mut events);

    drop(control);
    drop(events);
    drop(system);

    let lines = sink.lines();
    let toolcall = find_enter_with_field(&lines, "toolcall", "tool", "trace_echo");
    let iteration = assert_parent_named(&lines, toolcall, "iteration_loop");
    let iteration_id = span_id(iteration);
    let chat = find_child_with_field(&lines, iteration_id, "api.chat", "purpose", "iteration");

    assert_eq!(
        token(toolcall, "iteration"),
        token(iteration, "iteration"),
        "toolcall task must seed the originating iteration context: {toolcall}"
    );
    for key in ["system", "session", "turn", "agent", "iteration"] {
        assert!(
            token(toolcall, key).is_some(),
            "toolcall task is missing run.{key}: {toolcall}"
        );
    }
    assert!(
        token(toolcall, "task").is_some_and(|task| task.starts_with("toolcall-")),
        "toolcall must use an independent logical task: {toolcall}"
    );

    let arguments_event = find_event(&lines, span_id(toolcall), "arguments");
    let expected_argument_bytes = arguments.len().to_string();
    assert_eq!(
        token(arguments_event, "argument_bytes"),
        Some(expected_argument_bytes.as_str()),
        "arguments event must contain only the payload size"
    );
    let result = find_event(&lines, span_id(toolcall), "result");
    assert_eq!(token(result, "ok"), Some("true"), "{result}");
    assert_eq!(token(result, "blocked"), Some("false"), "{result}");

    for span in [toolcall, iteration, chat] {
        assert_has_exit(&lines, span);
    }

    let trace = lines.join("\n");
    for secret in [ARGUMENT_PAYLOAD_SECRET, TOOL_OUTPUT_SECRET] {
        assert!(
            !trace.contains(secret),
            "trace leaked payload marker {secret:?}: {trace}"
        );
    }
}

fn assistant_tool_call(arguments_json: &str) -> String {
    json!({
        "choices": [{
            "message": {
                "role": "assistant",
                "tool_calls": [{
                    "id": "call_trace_1",
                    "type": "function",
                    "function": {
                        "name": "trace_echo",
                        "arguments": arguments_json,
                    },
                }],
            },
        }],
    })
    .to_string()
}

fn assert_parent_named<'a>(lines: &'a [String], child: &str, expected: &str) -> &'a str {
    let parent_id = token(child, "parent")
        .filter(|parent| *parent != "none")
        .unwrap_or_else(|| panic!("span has no structural parent: {child}"));
    let parent = find_enter_by_id(lines, parent_id);
    assert_eq!(
        token(parent, "span-name"),
        Some(expected),
        "unexpected parent for {child}"
    );
    parent
}

fn find_enter_with_field<'a>(lines: &'a [String], name: &str, field: &str, value: &str) -> &'a str {
    lines
        .iter()
        .find(|line| {
            line_type(line) == Some("enter")
                && token(line, "span-name") == Some(name)
                && token(line, field) == Some(value)
        })
        .map(String::as_str)
        .unwrap_or_else(|| panic!("missing {name} span with {field}={value}: {lines:#?}"))
}

fn find_child_with_field<'a>(
    lines: &'a [String],
    parent_id: &str,
    name: &str,
    field: &str,
    value: &str,
) -> &'a str {
    lines
        .iter()
        .find(|line| {
            line_type(line) == Some("enter")
                && token(line, "parent") == Some(parent_id)
                && token(line, "span-name") == Some(name)
                && token(line, field) == Some(value)
        })
        .map(String::as_str)
        .unwrap_or_else(|| {
            panic!("missing {name} child of span {parent_id} with {field}={value}: {lines:#?}")
        })
}

fn find_enter_by_id<'a>(lines: &'a [String], id: &str) -> &'a str {
    lines
        .iter()
        .find(|line| line_type(line) == Some("enter") && token(line, "span") == Some(id))
        .map(String::as_str)
        .unwrap_or_else(|| panic!("missing enter record for span {id}: {lines:#?}"))
}

fn find_event<'a>(lines: &'a [String], span: &str, name: &str) -> &'a str {
    lines
        .iter()
        .find(|line| {
            line_type(line) == Some("event")
                && token(line, "span") == Some(span)
                && token(line, "event-name") == Some(name)
        })
        .map(String::as_str)
        .unwrap_or_else(|| panic!("missing {name} event for span {span}: {lines:#?}"))
}

fn assert_has_exit(lines: &[String], enter: &str) {
    let id = span_id(enter);
    assert!(
        lines
            .iter()
            .any(|line| line_type(line) == Some("exit") && token(line, "span") == Some(id)),
        "missing exit record for span {id}: {enter}"
    );
}

fn span_id(line: &str) -> &str {
    token(line, "span").unwrap_or_else(|| panic!("line has no span id: {line}"))
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

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}
