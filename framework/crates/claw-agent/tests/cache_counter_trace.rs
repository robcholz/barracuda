#![allow(clippy::unwrap_used)]

mod support;

use std::sync::{Arc, Mutex, MutexGuard};

use claw_agent::Message;
use claw_log::{FlatTreeSubscriber, TraceSink};
use futures_lite::future::block_on;
use serde_json::json;
use tracing::Level;

use support::{build_mem_system, drain_until_turn_ended, mem_root, serialize_script};

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

#[test]
fn provider_usage_traces_cache_hit_rate_as_a_numeric_counter_sample() {
    let sink = RecordingSink::default();
    let subscriber = FlatTreeSubscriber::with_sink(sink.clone())
        .with_allowed_target_prefix("claw")
        .with_context_group_keys("run", ["system", "session", "turn", "agent", "iteration"]);
    tracing::subscriber::set_global_default(subscriber)
        .expect("this single-test binary installs tracing exactly once");

    let _script = serialize_script();
    let response = json!({
        "choices": [{ "message": { "role": "assistant", "content": "done" } }],
        "usage": {
            "prompt_tokens": 100,
            "completion_tokens": 4,
            "prompt_tokens_details": { "cached_tokens": 75 }
        }
    })
    .to_string();
    let system = build_mem_system(&mem_root("cache-counter-trace"), vec![response]);
    let session = system
        .new_session(claw_agent::SessionPersistence::Persistent)
        .unwrap();
    let (control, mut events) = system.open_session(session).unwrap();

    block_on(control.append(Message::text("report cache usage"))).unwrap();
    let _ = drain_until_turn_ended(&mut events);

    drop(control);
    drop(events);
    drop(system);

    let lines = sink.lines();
    let sample = lines
        .iter()
        .find(|line| token(line, "event-name") == Some("context_cache_hit_rate"))
        .unwrap_or_else(|| panic!("missing context cache counter sample: {lines:#?}"));

    assert_eq!(token(sample, "input_tokens"), Some("100"));
    assert_eq!(token(sample, "cache_read_tokens"), Some("75"));
    assert_eq!(token(sample, "counter.value"), Some("75"));

    let span = token(sample, "span").unwrap();
    assert_ne!(span, "none");
    assert!(lines.iter().any(|line| {
        token(line, "span") == Some(span)
            && token(line, "span-name") == Some("iteration_loop")
            && line_type(line) == Some("enter")
    }));
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn line_type(line: &str) -> Option<&str> {
    line.split_whitespace()
        .skip_while(|part| *part != "TRACE")
        .nth(2)
}

fn token<'a>(line: &'a str, key: &str) -> Option<&'a str> {
    line.split_whitespace().find_map(|part| {
        let part = part.trim_matches(['<', '>']);
        part.strip_prefix(key)?.strip_prefix('=')
    })
}
