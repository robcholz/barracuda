//! The Inkbox receive loop against the scripted iMessage list, with sends
//! on a route of their own.

use alloc::format;
use alloc::string::ToString;
use alloc::vec;
use std::sync::mpsc::{sync_channel, SyncSender};

use barracuda_imessage_gateway_plugin::PAIRED_REPLY;
use barracuda_workflow_plugin::{
    workflow_action_schema_inline, WorkflowActionFuture, WorkflowActionHandler,
    WorkflowActionRegistry, WorkflowActionSchema, WorkflowService,
};
use embassy_executor::{Executor, Spawner};
use rusty_fork::rusty_fork_test;
use serde_json::{json, Value};

use super::*;
use crate::receive::{CURSOR_STORAGE_KEY, MAX_BODY_BYTES};

const OWNER: &str = "+15550000001";

fn message(id: &str, direction: &str, from: &str, text: Option<&str>, at: &str) -> Value {
    json!({
        "id": id,
        "conversation_id": format!("conv-{from}"),
        "reply_to_message_id": null,
        "thread_id": "thread",
        "direction": direction,
        "remote_number": from,
        "sender_number": null,
        "is_group": false,
        "content": text,
        "message_type": "message",
        "service": "imessage",
        "media": null,
        "status": "received",
        "is_read": false,
        "reactions": null,
        "created_at": at,
        "updated_at": at,
    })
}

fn list(messages: &[Value]) -> ScriptStep {
    ScriptStep::json(200, &Value::Array(messages.to_vec()).to_string())
}

/// A response with extra headers, kept alive.
fn raw(status: u16, headers: &str, body: &str) -> ScriptStep {
    let bytes = format!(
        "HTTP/1.1 {status} Error\r\nContent-Type: application/json\r\nContent-Length: {}\r\n{headers}Connection: keep-alive\r\n\r\n{body}",
        body.len()
    )
    .into_bytes();
    ScriptStep::Response {
        bytes,
        max_read: usize::MAX,
        pending_after: false,
    }
}

/// A poll the server never answers.
fn idle() -> ScriptStep {
    ScriptStep::pending_after_headers(200, "application/json")
}

// --- Polling, cursor, and errors -------------------------------------------

rusty_fork_test! {
    /// Runs in a process of its own: the Gateway's receive buffers are
    /// static, so each test needs a fresh process to get them.
    #[test]
    fn the_first_poll_starts_after_the_newest_message_on_one_connection() {
        let old = || {
            list(&[message(
                "old",
                "inbound",
                "+1555",
                Some(PAIRING_CODE),
                "2026-10-09T01:00:00.123456+00:00",
            )])
        };
        run(
            Setup {
                upstream: vec![old(), old(), idle()],
                ..Setup::configured("send_receive")
            },
            |harness| {
                harness.run_until("three polls", || harness.polls().len() >= 3);
                let polls = harness.polls();
                // The first poll only finds where the identity's messages end.
                assert_eq!(polls[0], "/api/v1/imessage/messages?limit=1");
                assert_eq!(
                    polls[1],
                    "/api/v1/imessage/messages?limit=50&start_datetime=2026-10-09T01%3A00%3A00.123456%2B00%3A00"
                );
                let requests = harness.requests();
                assert!(requests[0].contains("X-API-Key: key-1\r\n"));
                // One kept-alive connection serves every poll.
                assert_eq!(harness.bench.upstream.connect_count(), 1);
                assert_eq!(harness.state(), ReceiveState::Receiving);
                assert_eq!(
                    harness.status()["receive"],
                    json!({"state": "receiving", "slots": {"in_use": 1, "capacity": harness.capacity}})
                );
                // The baseline cursor is stored at once.
                assert_eq!(
                    harness.stored(CURSOR_STORAGE_KEY),
                    Some(json!({"start": "2026-10-09T01:00:00.123456+00:00", "recent": ["old"]}))
                );
                // The old message was never handled: nobody asked to pair with it.
                assert!(harness.sent().is_empty());
            },
        );
    }
}

rusty_fork_test! {
    /// Runs in a process of its own: the Gateway's receive buffers are
    /// static, so each test needs a fresh process to get them.
    #[test]
    fn an_empty_inbox_starts_from_the_epoch() {
        run(
            Setup {
                upstream: vec![list(&[]), idle()],
                ..Setup::configured("send_receive")
            },
            |harness| {
                harness.run_until("a second poll", || harness.polls().len() >= 2);
                assert_eq!(
                    harness.polls()[1],
                    "/api/v1/imessage/messages?limit=50&start_datetime=1970-01-01T00%3A00%3A00Z"
                );
            },
        );
    }
}

rusty_fork_test! {
    /// Runs in a process of its own: the Gateway's receive buffers are
    /// static, so each test needs a fresh process to get them.
    #[test]
    fn the_cursor_is_restored_and_handled_ids_are_dropped() {
        run(
            Setup {
                stored: vec![(
                    CURSOR_STORAGE_KEY,
                    br#"{"start":"2026-10-09T01:02:03Z","recent":["seen"]}"#.to_vec(),
                )],
                upstream: vec![
                    list(&[
                        message(
                            "mine",
                            "outbound",
                            "+1555",
                            Some("sent"),
                            "2026-10-09T01:05:00Z",
                        ),
                        message(
                            "seen",
                            "inbound",
                            "+1555",
                            Some(PAIRING_CODE),
                            "2026-10-09T01:02:03Z",
                        ),
                    ]),
                    idle(),
                ],
                ..Setup::configured("send_receive")
            },
            |harness| {
                harness.run_until("two polls", || harness.polls().len() >= 2);
                assert!(harness.polls()[0].ends_with("&start_datetime=2026-10-09T01%3A02%3A03Z"));
                // "seen" is in the restored ring and is not handled again; the
                // outbound message moves the cursor but is not handled.
                harness.run_until("the cursor", || {
                    harness
                        .stored(CURSOR_STORAGE_KEY)
                        .is_some_and(|cursor| cursor["start"] == "2026-10-09T01:05:00Z")
                });
                assert!(harness.sent().is_empty());
                let cursor = harness.stored(CURSOR_STORAGE_KEY).unwrap();
                assert_eq!(cursor["recent"], json!(["seen", "mine"]));
            },
        );
    }
}

rusty_fork_test! {
    /// Runs in a process of its own: the Gateway's receive buffers are
    /// static, so each test needs a fresh process to get them.
    #[test]
    fn a_rate_limit_waits_for_retry_after() {
        run(
            Setup {
                stored: vec![(
                    CURSOR_STORAGE_KEY,
                    br#"{"start":"2026-10-09T01:02:03Z"}"#.to_vec(),
                )],
                upstream: vec![
                    raw(429, "Retry-After: 7\r\n", r#"{"detail":"slow down"}"#),
                    idle(),
                ],
                ..Setup::configured("send_receive")
            },
            |harness| {
                harness.run_until("the rate limit", || {
                    matches!(harness.state(), ReceiveState::Error(_))
                });
                assert_eq!(
                    harness.state(),
                    ReceiveState::Error("Inkbox rate limit".into())
                );
                // Retry-After: 7 holds the next poll back far longer than the backoff.
                harness.run_for(Duration::from_millis(200));
                assert_eq!(harness.polls().len(), 1);
            },
        );
    }
}

rusty_fork_test! {
    /// Runs in a process of its own: the Gateway's receive buffers are
    /// static, so each test needs a fresh process to get them.
    #[test]
    fn a_rejected_api_key_halts_receiving() {
        run(
            Setup {
                upstream: vec![ScriptStep::json(401, r#"{"detail":"Invalid API key"}"#)],
                ..Setup::configured("send_receive")
            },
            |harness| {
                harness.run_until("halted", || {
                    matches!(harness.state(), ReceiveState::Error(_))
                });
                harness.run_for(Duration::from_millis(100));
                assert_eq!(harness.polls().len(), 1);
                assert!(harness
                    .state()
                    .message()
                    .unwrap()
                    .contains("Inkbox rejected the API key"));
                assert_eq!(harness.status()["receive"]["state"], "error");
            },
        );
    }
}

rusty_fork_test! {
    /// Runs in a process of its own: the Gateway's receive buffers are
    /// static, so each test needs a fresh process to get them.
    #[test]
    fn a_page_over_the_body_limit_is_asked_for_again_smaller() {
        let long = "x".repeat(MAX_BODY_BYTES);
        run(
            Setup {
                stored: vec![(
                    CURSOR_STORAGE_KEY,
                    br#"{"start":"2026-10-09T01:02:03Z"}"#.to_vec(),
                )],
                upstream: vec![
                    list(&[message(
                        "big",
                        "inbound",
                        "+1555",
                        Some(&long),
                        "2026-10-09T01:05:00Z",
                    )]),
                    list(&[]),
                    idle(),
                ],
                ..Setup::configured("send_receive")
            },
            |harness| {
                harness.run_until("a smaller page", || harness.polls().len() >= 2);
                assert!(harness.polls()[0].contains("limit=50&"));
                assert!(harness.polls()[1].contains("limit=25&"));
                harness.run_until("receiving", || harness.state() == ReceiveState::Receiving);
            },
        );
    }
}

// --- Modes and slots -------------------------------------------------------

rusty_fork_test! {
    /// Runs in a process of its own: the Gateway's receive buffers are
    /// static, so each test needs a fresh process to get them.
    #[test]
    fn modes_stop_and_restart_polling() {
        run(
            Setup {
                stored: vec![(
                    CURSOR_STORAGE_KEY,
                    br#"{"start":"2026-10-09T01:02:03Z"}"#.to_vec(),
                )],
                upstream: vec![list(&[]), list(&[]), idle(), list(&[]), idle()],
                ..Setup::configured("send_receive")
            },
            |harness| {
                harness.run_until("receiving", || harness.state() == ReceiveState::Receiving);
                assert_eq!(harness.set_mode("send").status, 204);
                harness.run_until("idle", || harness.state() == ReceiveState::Idle);
                assert_eq!(harness.channel.in_use(), 0);
                let polls = harness.polls().len();
                harness.run_for(Duration::from_millis(60));
                assert_eq!(harness.polls().len(), polls);
                assert_eq!(
                    harness.status(),
                    json!({"configured": true, "mode": "send", "owners": {"count": 0}})
                );
                assert_eq!(harness.set_mode("send_receive").status, 204);
                harness.run_until("polling again", || harness.polls().len() > polls);
            },
        );
    }
}

rusty_fork_test! {
    /// Runs in a process of its own: the Gateway's receive buffers are
    /// static, so each test needs a fresh process to get them.
    #[test]
    fn a_configuration_stored_before_modes_sends_only() {
        run(
            Setup {
                mode: None,
                ..Setup::configured("send")
            },
            |harness| {
                assert_eq!(harness.channel.mode(), ChannelMode::Send);
                harness.run_for(Duration::from_millis(50));
                assert!(harness.polls().is_empty());
                assert_eq!(harness.stored("mode"), Some(json!("send")));
            },
        );
    }
}

rusty_fork_test! {
    /// Runs in a process of its own: the Gateway's receive buffers are
    /// static, so each test needs a fresh process to get them.
    #[test]
    fn a_full_pool_waits_for_a_slot() {
        run(
            Setup {
                hold_slots: true,
                upstream: vec![list(&[]), idle()],
                ..Setup::configured("send_receive")
            },
            |harness| {
                assert_eq!(harness.state(), ReceiveState::NoSlot);
                harness.run_for(Duration::from_millis(60));
                assert!(harness.polls().is_empty());
                drop(harness.held.take());
                harness.run_until("receiving", || harness.state() == ReceiveState::Receiving);
            },
        );
    }
}

// --- Owners, publishing, and dedup with Workflow running --------------------

/// Records every `gateway.message.received` a Workflow hands it.
struct Record(Rc<RefCell<Vec<Value>>>);

impl WorkflowActionHandler for Record {
    type Request = Value;
    type Response = Value;

    const SCHEMA: WorkflowActionSchema = workflow_action_schema_inline!(
        "test.record",
        r#"{"type":"object","properties":{"route":{"type":"object"},"message_id":{"type":"string"},"text":{"type":"string"}}}"#,
        r#"{"type":"object"}"#
    );

    fn invoke(&self, request: Value) -> WorkflowActionFuture<'_, Value> {
        self.0.borrow_mut().push(request);
        Box::pin(async { Ok(json!({})) })
    }
}

const RECORD_WORKFLOW: &str = r#"{
    "id": "record-inbound",
    "match": {"event": "gateway.message.received"},
    "steps": [{"call": "test.record", "arguments": {
        "route": "$event.input.route",
        "message_id": "$event.input.message_id",
        "text": "$event.input.text"
    }}]
}"#;

struct Recorder(Rc<RefCell<Vec<Value>>>);

impl PluginDeclaration for Recorder {
    const ID: &'static str = "inbound-recorder";
    const DEPENDS_ON: &'static [&'static str] = &["workflow"];
}

impl Plugin for Recorder {
    fn register<Storage>(
        &mut self,
        context: &mut PluginRegisterContext<'_, Storage>,
    ) -> PluginResult<()>
    where
        Storage: PluginStorage,
    {
        let actions = context.require::<WorkflowActionRegistry>("workflow")?;
        let service = context.require::<WorkflowService>("workflow")?;
        context.retain(
            actions
                .add_action(Record(Rc::clone(&self.0)))
                .map_err(PluginError::registration)?,
        );
        embassy_futures::block_on(service.load_transient(RECORD_WORKFLOW))
            .map_err(PluginError::registration)?;
        Ok(())
    }
}

/// Runs the bench's network and Inkbox API on the executor.
#[allow(
    clippy::await_holding_refcell_ref,
    reason = "the end-to-end test only waits on the executor, never through `Bench::drive`"
)]
#[embassy_executor::task]
async fn run_bench(bench: Rc<Bench>) {
    let mut driver = bench.driver.borrow_mut();
    driver.as_mut().await;
}

#[embassy_executor::task]
async fn end_to_end(spawner: Spawner, done: SyncSender<Result<(), String>>) {
    let outcome = end_to_end_steps(spawner).await;
    let _sent = done.send(outcome);
}

async fn end_to_end_steps(spawner: Spawner) -> Result<(), String> {
    install_global_memory_vfs()
        .await
        .map_err(|error| error.to_string())?;
    let partition = memory_partition(64 * 1024)
        .await
        .map_err(|error| error.to_string())?;
    let mut manager = PluginManager::open(partition)
        .await
        .map_err(|error| error.to_string())?;
    manager.install_vfs(barracuda_vfs::global_namespace().await);
    manager.install_task_spawner(spawner);
    let network = loopback_network();
    let mut context = plugin_context(&network);
    let records = Rc::new(RefCell::new(Vec::new()));
    register_services(&mut manager, &mut context);
    manager
        .register(Recorder(Rc::clone(&records)))
        .map_err(|error| error.to_string())?;
    let owner = format!(r#"[{{"id":"{OWNER}"}}]"#).into_bytes();
    let at = |second: u32| format!("2026-10-09T01:00:{second:02}Z");
    let hello = message("m1", "inbound", OWNER, Some(" hello "), &at(1));
    let stranger = message("m2", "inbound", "+1666", Some("hi"), &at(2));
    let code = message("m3", "inbound", "+1777", Some(PAIRING_CODE), &at(3));
    let photo = message("m4", "inbound", OWNER, None, &at(4));
    let again = message("m5", "inbound", OWNER, Some("again"), &at(5));
    let mut setup = Setup {
        stored: vec![
            ("owners", owner),
            (
                CURSOR_STORAGE_KEY,
                br#"{"start":"2026-10-09T01:00:00Z"}"#.to_vec(),
            ),
        ],
        upstream: vec![
            list(&[photo.clone(), code.clone(), stranger.clone(), hello.clone()]),
            // The connection drops; the next poll repeats the newest.
            ScriptStep::ConnectError(embedded_io_async::ErrorKind::ConnectionReset),
            list(&[again.clone(), photo.clone()]),
            idle(),
        ],
        sends: Some(vec![ScriptStep::json(
            201,
            r#"{"message":{"id":"reply-1"}}"#,
        )]),
        ..Setup::configured("send_receive")
    };
    let bench = Bench::new(network, &mut setup);
    spawner.spawn(run_bench(Rc::clone(&bench)).map_err(|error| format!("{error:?}"))?);
    let harness =
        register_probe(&mut manager, setup, bench, true).map_err(|error| error.to_string())?;
    let pairing = harness.channel.pairing_code().ok_or("pairing code")?;
    if pairing != PAIRING_CODE {
        return Err(format!("pairing code {pairing}"));
    }
    manager.start().map_err(|error| error.to_string())?;

    let finished = with_timeout(Duration::from_secs(5), async {
        while records.borrow().len() < 2 || harness.requests().len() < 3 {
            Timer::after_millis(5).await;
        }
    })
    .await;
    if finished.is_err() {
        return Err(format!(
            "timed out: records {:?}, polls {:?}",
            records.borrow(),
            harness.polls()
        ));
    }
    Timer::after_millis(20).await;

    let records = records.borrow().clone();
    let check = |condition: bool, what: &str| {
        if condition {
            Ok(())
        } else {
            Err(format!(
                "{what}: records {records:?}, polls {:?}, sent {:?}",
                harness.polls(),
                harness.sent()
            ))
        }
    };
    check(
        records
            == vec![
                json!({"route": {"channel": "inkbox", "conversation_id": format!("conv-{OWNER}")}, "message_id": "m1", "text": "hello"}),
                json!({"route": {"channel": "inkbox", "conversation_id": format!("conv-{OWNER}")}, "message_id": "m5", "text": "again"}),
            ],
        "owner messages are published once each, oldest first",
    )?;
    check(
        harness.polls()[1].ends_with("&start_datetime=2026-10-09T01%3A00%3A04Z"),
        "the cursor follows the newest message",
    )?;
    let sent = harness.sent();
    check(sent.len() == 1, "one paired reply")?;
    let (head, body) = sent[0].split_once("\r\n\r\n").ok_or("request body")?;
    let body: Value = serde_json::from_str(body).map_err(|error| error.to_string())?;
    check(
        head.starts_with("POST /api/v1/imessage/messages?agent_identity_id=identity-1 "),
        "the reply goes through the send path",
    )?;
    check(
        body == json!({"conversation_id": "conv-+1777", "text": PAIRED_REPLY}),
        "the reply confirms the pairing in the sender's conversation",
    )?;
    check(
        harness.channel.owner_ids() == [OWNER, "+1777"],
        "the sender with the code paired",
    )?;
    check(harness.channel.ignored() == 1, "the stranger is counted")?;
    // The executor thread outlives the test; keep its Plugins running.
    core::mem::forget(manager);
    Ok(())
}

rusty_fork_test! {
    /// Runs in a process of its own: the Gateway's receive buffers are
    /// static, so each test needs a fresh process to get them.
    #[test]
    fn owners_publish_codes_pair_strangers_are_ignored_and_repeats_dropped() {
        let (done, result) = sync_channel(1);
        std::thread::Builder::new()
            .name("inkbox-end-to-end".into())
            .stack_size(8 * 1024 * 1024)
            .spawn(move || {
                let executor = Box::leak(Box::new(Executor::new()));
                executor.run(|spawner| {
                    spawner.spawn(end_to_end(spawner, done).expect("spawn the end-to-end test"));
                });
            })
            .expect("spawn the executor thread");
        result
            .recv_timeout(std::time::Duration::from_secs(20))
            .expect("the end-to-end test timed out")
            .expect("the end-to-end test failed");
    }
}
