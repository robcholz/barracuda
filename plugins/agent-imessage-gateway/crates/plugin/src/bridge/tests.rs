//! Drives the builtin `imessage-to-agent` and `imessage-control-to-agent`
//! Workflows against the bridge Actions.

#![allow(clippy::expect_used, clippy::panic)]

use alloc::{
    borrow::ToOwned,
    boxed::Box,
    format,
    rc::Rc,
    string::{String, ToString},
    vec::Vec,
};
use core::{
    cell::{Cell, RefCell},
    future::poll_fn,
    task::Poll,
};

use barracuda_imessage_gateway_plugin::{
    GatewayControlReceived, GatewayMessageReceived, GatewayRoute, SendSessionsRequest,
    SessionNotice, parse_command,
};
use barracuda_platform_test::{install_global_memory_vfs, memory_partition};
use barracuda_plugin::manager::{
    Plugin, PluginDeclaration, PluginManager, PluginRegisterContext, PluginResult, PluginStorage,
};
use barracuda_workflow_plugin::{
    WorkflowActionFuture, WorkflowActionHandler, WorkflowActionRegistry, WorkflowActionSchema,
    WorkflowRuntime, parse_definition, workflow_action_schema_inline,
};
use futures_lite::future::{block_on, poll_once};
use serde_json::{Value, json};

use super::ImessageBridge;
use crate::sessions::{LocalFuture, RenameFailure, SessionMeta, SessionReplies, SessionStore};

/// The Workflow file that ships the bridge's inbound route.
const WORKFLOWS: &str = include_str!("../../../../../agent/filesystem/resources/workflows.json");

/// Polls of the Workflow Runtime a `session.new` takes, so a second inbound
/// message is dispatched while the first is still creating its session.
const SESSION_NEW_POLLS: usize = 3;

#[derive(Default)]
struct Trace {
    created: Cell<u32>,
    appended: RefCell<Vec<(String, String)>>,
    controls: RefCell<Vec<(&'static str, String)>>,
    persistence: RefCell<Vec<String>>,
    deleted: RefCell<Vec<String>>,
    replies: RefCell<Vec<SendSessionsRequest>>,
}

impl Trace {
    fn starting_at(created: u32) -> Self {
        Self {
            created: Cell::new(created),
            ..Self::default()
        }
    }
}

/// The Agent's sessions as the stubs made them: `session-N` titled `chat N`,
/// newer ones used later.
struct FakeSessions(Rc<Trace>);

impl SessionStore for FakeSessions {
    fn describe(&self) -> LocalFuture<'_, Vec<SessionMeta>> {
        let sessions = (1..=self.0.created.get())
            .map(|number| format!("session-{number}"))
            .filter(|session| !self.0.deleted.borrow().contains(session))
            .map(|session| SessionMeta {
                title: Some(session.replace("session-", "chat ")),
                updated_at: session
                    .strip_prefix("session-")
                    .and_then(|number| number.parse::<u64>().ok())
                    .map(|number| number * 1000),
                session,
            })
            .collect();
        Box::pin(async move { sessions })
    }

    fn rename<'a>(
        &'a self,
        _session: &'a str,
        _title: &'a str,
    ) -> LocalFuture<'a, Result<(), RenameFailure>> {
        Box::pin(async { Ok(()) })
    }

    fn delete<'a>(&'a self, session: &'a str) -> LocalFuture<'a, Result<(), ()>> {
        self.0.deleted.borrow_mut().push(session.to_owned());
        Box::pin(async { Ok(()) })
    }
}

impl SessionReplies for FakeSessions {
    fn send(&self, request: SendSessionsRequest) -> LocalFuture<'_, ()> {
        self.0.replies.borrow_mut().push(request);
        Box::pin(async {})
    }

    fn now(&self) -> Option<u64> {
        None
    }
}

fn fakes(trace: &Rc<Trace>) -> (Rc<dyn SessionStore>, Rc<dyn SessionReplies>) {
    (
        Rc::new(FakeSessions(Rc::clone(trace))),
        Rc::new(FakeSessions(Rc::clone(trace))),
    )
}

async fn yield_now(times: usize) {
    let mut remaining = times;
    poll_fn(|context| {
        if remaining == 0 {
            Poll::Ready(())
        } else {
            remaining = remaining.saturating_sub(1);
            context.waker().wake_by_ref();
            Poll::Pending
        }
    })
    .await;
}

struct SessionNew(Rc<Trace>);

impl WorkflowActionHandler for SessionNew {
    type Request = Value;
    type Response = Value;

    const SCHEMA: WorkflowActionSchema = workflow_action_schema_inline!(
        "session.new",
        r#"{"type":"object"}"#,
        r#"{"type":"object"}"#
    );

    fn invoke(&self, request: Value) -> WorkflowActionFuture<'_, Value> {
        self.0.persistence.borrow_mut().push(
            request["persistence"]
                .as_str()
                .unwrap_or_default()
                .to_owned(),
        );
        Box::pin(async move {
            yield_now(SESSION_NEW_POLLS).await;
            let created = self.0.created.get().saturating_add(1);
            self.0.created.set(created);
            Ok(json!({ "session": format!("session-{created}") }))
        })
    }
}

struct SessionOpen;

impl WorkflowActionHandler for SessionOpen {
    type Request = Value;
    type Response = Value;

    const SCHEMA: WorkflowActionSchema = workflow_action_schema_inline!(
        "session.open",
        r#"{"type":"object"}"#,
        r#"{"type":"object"}"#
    );

    fn invoke(&self, request: Value) -> WorkflowActionFuture<'_, Value> {
        Box::pin(async move { Ok(json!({ "session": request["session"], "run": "run-1" })) })
    }
}

struct SessionAppend(Rc<Trace>);

impl WorkflowActionHandler for SessionAppend {
    type Request = Value;
    type Response = Value;

    const SCHEMA: WorkflowActionSchema = workflow_action_schema_inline!(
        "session.append",
        r#"{"type":"object"}"#,
        r#"{"type":"object"}"#
    );

    fn invoke(&self, request: Value) -> WorkflowActionFuture<'_, Value> {
        let session = request["session"].as_str().unwrap_or_default().to_owned();
        let text = request["text"].as_str().unwrap_or_default().to_owned();
        self.0.appended.borrow_mut().push((session, text));
        Box::pin(async { Ok(json!({})) })
    }
}

/// Declares a stub session control Action that records the session it acts on.
macro_rules! control_action {
    ($name:ident, $address:literal) => {
        struct $name(Rc<Trace>);

        impl WorkflowActionHandler for $name {
            type Request = Value;
            type Response = Value;

            const SCHEMA: WorkflowActionSchema = workflow_action_schema_inline!(
                $address,
                r#"{"type":"object"}"#,
                r#"{"type":"object"}"#
            );

            fn invoke(&self, request: Value) -> WorkflowActionFuture<'_, Value> {
                let session = request["session"].as_str().unwrap_or_default().to_owned();
                self.0.controls.borrow_mut().push(($address, session));
                Box::pin(async { Ok(json!({})) })
            }
        }
    };
}

control_action!(SessionInterrupt, "session.interrupt");
control_action!(SessionCancel, "session.cancel");

/// Declares a stub Action that fails the test when the Workflow takes a branch
/// the race must not reach.
macro_rules! unexpected_action {
    ($name:ident, $address:literal) => {
        struct $name;

        impl WorkflowActionHandler for $name {
            type Request = Value;
            type Response = Value;

            const SCHEMA: WorkflowActionSchema = workflow_action_schema_inline!(
                $address,
                r#"{"type":"object"}"#,
                r#"{"type":"object"}"#
            );

            fn invoke(&self, request: Value) -> WorkflowActionFuture<'_, Value> {
                panic!("unexpected `{}` call: {request}", $address);
            }
        }
    };
}

unexpected_action!(SessionRespond, "session.respond");
unexpected_action!(GatewaySendStream, "gateway.send_stream");

/// Runs the race inside Plugin registration, where scoped storage exists.
struct RaceProbe {
    completed: Rc<Cell<bool>>,
}

impl PluginDeclaration for RaceProbe {
    const ID: &'static str = "imessage-bridge-race-probe";
    const DEPENDS_ON: &'static [&'static str] = &[];
}

impl Plugin for RaceProbe {
    fn register<Storage>(
        &mut self,
        context: &mut PluginRegisterContext<'_, Storage>,
    ) -> PluginResult<()>
    where
        Storage: PluginStorage,
    {
        let bridge =
            block_on(ImessageBridge::load(context.storage().clone())).expect("load the bridge");
        two_quick_messages_on_a_new_route_share_one_session(&bridge);
        session_commands_list_switch_and_delete(&bridge);
        self.completed.set(true);
        Ok(())
    }
}

fn two_quick_messages_on_a_new_route_share_one_session<Storage: PluginStorage>(
    bridge: &ImessageBridge<Storage>,
) {
    let trace = Rc::new(Trace::default());
    let actions = WorkflowActionRegistry::new();
    let (store, replies) = fakes(&trace);
    let _bridge_actions = bridge
        .register_actions(&actions, store, replies)
        .expect("register bridge Actions");
    let _stubs = [
        actions.add_action(SessionNew(Rc::clone(&trace))),
        actions.add_action(SessionOpen),
        actions.add_action(SessionAppend(Rc::clone(&trace))),
        actions.add_action(SessionRespond),
        actions.add_action(GatewaySendStream),
        actions.add_action(SessionInterrupt(Rc::clone(&trace))),
        actions.add_action(SessionCancel(Rc::clone(&trace))),
    ]
    .map(|registration| registration.expect("register stub Action"));
    let mut runtime = WorkflowRuntime::new(actions);
    let control = runtime.control();
    let workflows: Vec<Value> = serde_json::from_str(WORKFLOWS).expect("Workflow file");
    for id in ["imessage-to-agent", "imessage-control-to-agent"] {
        let workflow = workflows
            .iter()
            .find(|workflow| workflow["id"] == id)
            .expect("bundled Workflow");
        control
            .load(parse_definition(&workflow.to_string()).expect("parse Workflow"))
            .expect("load Workflow");
    }

    for (message_id, text) in [("message-1", "first"), ("message-2", "second")] {
        control
            .emit::<GatewayMessageReceived>(json!({
                "route": { "channel": "bluebubbles", "conversation_id": "chat-7" },
                "message_id": message_id,
                "text": text,
            }))
            .expect("emit inbound message");
    }
    block_on(async {
        for _poll in 0..64 {
            let _pending = poll_once(&mut runtime).await;
            if runtime.view().info().completed_count == 2 {
                break;
            }
        }
    });

    let info = runtime.view().info();
    assert_eq!(info.failed_count, 0, "{:?}", info.last_failure);
    assert_eq!(info.completed_count, 2);
    assert_eq!(trace.created.get(), 1, "one session for the new route");
    assert_eq!(
        *trace.appended.borrow(),
        [
            ("session-1".to_owned(), "first".to_owned()),
            ("session-1".to_owned(), "second".to_owned()),
        ]
    );

    // stop and rewind reach the route's session; a route with none is left alone
    for (conversation, kind) in [
        ("chat-7", "interrupt"),
        ("chat-7", "cancel"),
        ("chat-8", "cancel"),
    ] {
        control
            .emit::<GatewayControlReceived>(json!({
                "route": { "channel": "bluebubbles", "conversation_id": conversation },
                "control": kind,
            }))
            .expect("emit control");
    }
    block_on(async {
        for _poll in 0..64 {
            let _pending = poll_once(&mut runtime).await;
            if runtime.view().info().completed_count == 5 {
                break;
            }
        }
    });

    let info = runtime.view().info();
    assert_eq!(info.failed_count, 0, "{:?}", info.last_failure);
    assert_eq!(info.completed_count, 5);
    assert_eq!(
        *trace.controls.borrow(),
        [
            ("session.interrupt", "session-1".to_owned()),
            ("session.cancel", "session-1".to_owned()),
        ]
    );
    assert_eq!(trace.created.get(), 1, "a control never creates a session");
}

/// Polls the Workflow Runtime until `count` executions completed.
fn drive(runtime: &mut WorkflowRuntime, count: usize) {
    block_on(async {
        for _poll in 0..128 {
            let _pending = poll_once(&mut *runtime).await;
            if runtime.view().info().completed_count >= count {
                break;
            }
        }
    });
    let info = runtime.view().info();
    assert_eq!(info.failed_count, 0, "{:?}", info.last_failure);
    assert_eq!(info.completed_count, count);
}

fn session_commands_list_switch_and_delete<Storage: PluginStorage>(
    bridge: &ImessageBridge<Storage>,
) {
    // numbered after the race's sessions, which another route owns
    let trace = Rc::new(Trace::starting_at(10));
    let actions = WorkflowActionRegistry::new();
    let (store, replies) = fakes(&trace);
    let _bridge_actions = bridge
        .register_actions(&actions, store, replies)
        .expect("register bridge Actions");
    let _stubs = [
        actions.add_action(SessionNew(Rc::clone(&trace))),
        actions.add_action(SessionOpen),
        actions.add_action(SessionAppend(Rc::clone(&trace))),
        actions.add_action(SessionRespond),
        actions.add_action(GatewaySendStream),
        actions.add_action(SessionInterrupt(Rc::clone(&trace))),
        actions.add_action(SessionCancel(Rc::clone(&trace))),
    ]
    .map(|registration| registration.expect("register stub Action"));
    let mut runtime = WorkflowRuntime::new(actions);
    let control = runtime.control();
    let workflows: Vec<Value> = serde_json::from_str(WORKFLOWS).expect("Workflow file");
    for id in ["imessage-to-agent", "imessage-control-to-agent"] {
        let workflow = workflows
            .iter()
            .find(|workflow| workflow["id"] == id)
            .expect("bundled Workflow");
        control
            .load(parse_definition(&workflow.to_string()).expect("parse Workflow"))
            .expect("load Workflow");
    }
    let route = GatewayRoute::new("telegram", "chat-9");
    let mut done = 0;
    // as the Gateway's ingress does: a session command becomes a control
    let mut message = |text: &str| {
        done += 1;
        match parse_command(&route, text) {
            Some(command) => control
                .emit::<GatewayControlReceived>(
                    serde_json::to_value(command).expect("encode control"),
                )
                .expect("emit control"),
            None => control
                .emit::<GatewayMessageReceived>(json!({
                    "route": route, "message_id": format!("message-{done}"), "text": text,
                }))
                .expect("emit message"),
        }
        drive(&mut runtime, done);
    };
    let last = |trace: &Rc<Trace>| trace.replies.borrow().last().cloned().expect("a reply");

    // a first message starts a saved session; a text command lists it
    message("plan the ride");
    message("/sessions");
    let listed = last(&trace);
    assert_eq!(listed.current.as_deref(), Some("session-11"));
    assert!(!listed.temporary && listed.notice.is_none());
    assert_eq!(listed.sessions.len(), 1);
    assert_eq!(listed.sessions[0].title.as_deref(), Some("chat 11"));
    assert_eq!(listed.target.channel, "telegram");

    // /new temp: the next message starts an ephemeral session, never listed
    message("/new temp");
    assert_eq!(
        last(&trace).notice,
        Some(SessionNotice::Created { temporary: true })
    );
    message("just between us");
    assert_eq!(*trace.persistence.borrow(), ["persistent", "ephemeral"]);
    message("/sessions");
    let listed = last(&trace);
    assert_eq!(listed.current.as_deref(), Some("session-12"));
    assert!(listed.temporary);
    assert_eq!(listed.sessions.len(), 1, "a temporary chat is not listed");

    // switching back deletes the temporary chat and continues the saved session
    message("/switch 1");
    let switched = last(&trace);
    assert_eq!(
        switched.notice,
        Some(SessionNotice::Switched {
            title: Some("chat 11".into())
        })
    );
    assert_eq!(switched.current.as_deref(), Some("session-11"));
    assert_eq!(*trace.deleted.borrow(), ["session-12"]);
    message("and the weather?");
    assert_eq!(
        trace.appended.borrow().last(),
        Some(&("session-11".to_owned(), "and the weather?".to_owned()))
    );

    // deleting asks first; confirmed, the conversation starts over
    message("/delete 1");
    assert_eq!(
        last(&trace).notice,
        Some(SessionNotice::ConfirmDelete {
            index: 1,
            title: Some("chat 11".into())
        })
    );
    message("/delete 1 confirm");
    let deleted = last(&trace);
    assert_eq!(deleted.current, None);
    assert!(deleted.sessions.is_empty());
    assert_eq!(*trace.deleted.borrow(), ["session-12", "session-11"]);
    message("/switch 3");
    assert_eq!(last(&trace).notice, Some(SessionNotice::UnknownSession));
    message("/rename 1");
    assert_eq!(last(&trace).notice, Some(SessionNotice::Usage));
    message("start again");
    assert_eq!(
        trace.persistence.borrow().last().map(String::as_str),
        Some("persistent")
    );
}

#[test]
fn quick_inbound_messages_on_a_new_route_bind_to_one_session() {
    block_on(install_global_memory_vfs()).expect("install test VFS");
    let partition = block_on(memory_partition(64 * 1024)).expect("create test database region");
    let mut manager = block_on(PluginManager::open(partition)).expect("open Plugin storage");
    manager.install_vfs(block_on(barracuda_vfs::global_namespace()));
    let completed = Rc::new(Cell::new(false));

    manager
        .register(RaceProbe {
            completed: Rc::clone(&completed),
        })
        .expect("run the race probe");

    assert!(completed.get());
}
