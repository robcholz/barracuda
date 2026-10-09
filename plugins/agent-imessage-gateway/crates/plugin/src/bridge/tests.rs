//! Drives the builtin `imessage-to-agent` Workflow against the bridge Actions.

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

use barracuda_imessage_gateway_plugin::GatewayMessageReceived;
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

/// The Workflow file that ships the bridge's inbound route.
const WORKFLOWS: &str = include_str!("../../../../../agent/filesystem/resources/workflows.json");

/// Polls of the Workflow Runtime a `session.new` takes, so a second inbound
/// message is dispatched while the first is still creating its session.
const SESSION_NEW_POLLS: usize = 3;

#[derive(Default)]
struct Trace {
    created: Cell<u32>,
    appended: RefCell<Vec<(String, String)>>,
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

    fn invoke(&self, _request: Value) -> WorkflowActionFuture<'_, Value> {
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
        self.completed.set(true);
        Ok(())
    }
}

fn two_quick_messages_on_a_new_route_share_one_session<Storage: PluginStorage>(
    bridge: &ImessageBridge<Storage>,
) {
    let trace = Rc::new(Trace::default());
    let actions = WorkflowActionRegistry::new();
    let _bridge_actions = bridge
        .register_actions(&actions)
        .expect("register bridge Actions");
    let _stubs = [
        actions.add_action(SessionNew(Rc::clone(&trace))),
        actions.add_action(SessionOpen),
        actions.add_action(SessionAppend(Rc::clone(&trace))),
        actions.add_action(SessionRespond),
        actions.add_action(GatewaySendStream),
    ]
    .map(|registration| registration.expect("register stub Action"));
    let mut runtime = WorkflowRuntime::new(actions);
    let control = runtime.control();
    let workflows: Vec<Value> = serde_json::from_str(WORKFLOWS).expect("Workflow file");
    let inbound = workflows
        .iter()
        .find(|workflow| workflow["id"] == "imessage-to-agent")
        .expect("imessage-to-agent Workflow");
    control
        .load(parse_definition(&inbound.to_string()).expect("parse imessage-to-agent"))
        .expect("load imessage-to-agent");

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
