//! End-to-end contract for the builtin iMessage Agent Workflows.

#![allow(clippy::expect_used)]

use std::cell::{Cell, RefCell};
use std::error::Error;
use std::fmt;
use std::future::{pending, poll_fn};
use std::pin::Pin;
use std::rc::Rc;
use std::string::{String, ToString};
use std::task::Poll;
use std::vec::Vec;

use barracuda_event_router::{
    Component, ComponentError, ComponentFuture, ComponentResult, Event, EventEmitter, EventRouter,
    JsonRef, JsonRpcSchema, JsonSchema, JsonWriter, RegisterContext, RpcLaneStorage, RunContext,
    UnregisterContext,
};
use barracuda_imessage_bridge_plugin::ImessageBridgePlugin;
use barracuda_platform_test::{install_global_memory_vfs, memory_partition, never_embassy_stack};
use barracuda_plugin_api::{ClientFactory, PluginContext};
use barracuda_plugin_manager::PluginManager;
use barracuda_vfs::{create_dir_all, write};

const FRAME_SIZE: usize = 512;
const ANY_JSON: JsonSchema = barracuda_event_router::json_schema_inline!("{}");
const WORKFLOWS: &str = include_str!("../../../../../image/system/workflows.json");

struct GatewayMessageReceived;

impl Event for GatewayMessageReceived {
    const ID: &'static str = "gateway.message.received";
}

struct SessionEvent;

impl Event for SessionEvent {
    const ID: &'static str = "session.event";
}

macro_rules! rpc {
    ($name:ident, $address:literal, $request:expr, $response:expr) => {
        struct $name;

        impl JsonRpcSchema for $name {
            const ADDRESS: &'static str = $address;
            const REQUEST_SCHEMA: JsonSchema = $request;
            const RESPONSE_SCHEMA: JsonSchema = $response;
            const MAX_REQUEST_BYTES: usize = FRAME_SIZE;
            const MAX_RESPONSE_BYTES: usize = FRAME_SIZE;
        }
    };
}

rpc!(NewSession, "session.new", ANY_JSON, ANY_JSON);
rpc!(OpenSession, "session.open", ANY_JSON, ANY_JSON);
rpc!(AppendSession, "session.append", ANY_JSON, ANY_JSON);
rpc!(SendStream, "gateway.send_stream", ANY_JSON, ANY_JSON);

#[derive(Debug)]
struct WorkflowTestError(&'static str);

impl fmt::Display for WorkflowTestError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.0)
    }
}

impl Error for WorkflowTestError {}

struct WorkflowDriver {
    appended: Rc<RefCell<Vec<String>>>,
    gateway_commands: Rc<RefCell<Vec<String>>>,
    open_calls: Rc<Cell<usize>>,
    completed: Rc<Cell<bool>>,
    failure: Rc<RefCell<Option<String>>>,
}

impl Component<FRAME_SIZE> for WorkflowDriver {
    fn name(&self) -> &'static str {
        "imessage-workflow-test-driver"
    }

    fn register(&mut self, context: &mut RegisterContext<'_, FRAME_SIZE>) -> ComponentResult<()> {
        context.register_json::<NewSession, _>(
            "system",
            |_context, _request, response: JsonWriter| async move {
                response.write(r#"{"session":"session-1"}"#).await
            },
        )?;
        let open_calls = Rc::clone(&self.open_calls);
        context.register_json::<OpenSession, _>(
            "system",
            move |_context, _request, response: JsonWriter| {
                let open_calls = Rc::clone(&open_calls);
                async move {
                    let call = open_calls.get();
                    open_calls.set(call.saturating_add(1));
                    if call == 0 {
                        response
                            .write(r#"{"session":"session-1","run":"run-1"}"#)
                            .await
                    } else {
                        response
                            .write(r#"{"session":"session-1","error":"worker_stopped"}"#)
                            .await
                    }
                }
            },
        )?;
        let appended = Rc::clone(&self.appended);
        context.register_json::<AppendSession, _>(
            "system",
            move |_context, request: JsonRef, response: JsonWriter| {
                let appended = Rc::clone(&appended);
                async move {
                    appended.borrow_mut().push(request.as_str()?.to_string());
                    response.write(r#"{"accepted_sequence":0}"#).await
                }
            },
        )?;
        let commands = Rc::clone(&self.gateway_commands);
        context.register_json::<SendStream, _>(
            "*",
            move |_context, request: JsonRef, response: JsonWriter| {
                let commands = Rc::clone(&commands);
                async move {
                    commands.borrow_mut().push(request.as_str()?.to_string());
                    response.write("{}").await
                }
            },
        )
    }

    fn run<'a>(&'a mut self, context: RunContext<FRAME_SIZE>) -> ComponentFuture<'a> {
        Box::pin(async move {
            let result = exercise_workflows(
                EventEmitter::new(context.rpc().clone()),
                &self.appended,
                &self.gateway_commands,
            )
            .await;
            if let Err(error) = result {
                self.failure.replace(Some(error.to_string()));
            }
            self.completed.set(true);
            pending().await
        })
    }

    fn unregister(&mut self, _context: &mut UnregisterContext<'_>) -> ComponentResult<()> {
        Ok(())
    }
}

async fn exercise_workflows(
    emitter: EventEmitter<FRAME_SIZE>,
    appended: &RefCell<Vec<String>>,
    gateway_commands: &RefCell<Vec<String>>,
) -> Result<(), ComponentError> {
    emitter
        .emit::<GatewayMessageReceived>(
            r#"{"route":{"channel":"imessage","conversation_id":"chat-1","thread_id":"thread-1"},"message_id":"message-1","text":"hello agent"}"#,
        )
        .await
        .map_err(ComponentError::lifecycle)?;
    wait_for_len(appended, 1, "inbound Workflow did not append to Agent").await?;

    emit_session_event(
        &emitter,
        0,
        "turn_started",
        r#"{"turn":"turn-1","origin":"user"}"#,
    )
    .await?;
    wait_for_len(
        gateway_commands,
        1,
        "outbound Workflow did not start stream",
    )
    .await?;

    emitter
        .emit::<GatewayMessageReceived>(
            r#"{"route":{"channel":"imessage","conversation_id":"chat-1","thread_id":"thread-1"},"message_id":"message-2","text":"queued message"}"#,
        )
        .await
        .map_err(ComponentError::lifecycle)?;
    wait_for_len(
        appended,
        2,
        "inbound Workflow did not append queued message",
    )
    .await?;

    emit_session_event(&emitter, 1, "reasoning_delta", r#"{"text":"private"}"#).await?;
    wait_for_len(
        gateway_commands,
        2,
        "outbound Workflow did not forward reasoning",
    )
    .await?;
    emit_session_event(&emitter, 2, "output_delta", r#"{"text":"hello user"}"#).await?;
    wait_for_len(
        gateway_commands,
        3,
        "outbound Workflow did not forward output",
    )
    .await?;
    emit_session_event(&emitter, 3, "turn_ended", r#"{"turn":"turn-1"}"#).await?;
    wait_for_len(
        gateway_commands,
        4,
        "outbound Workflow did not finish stream",
    )
    .await?;

    emit_session_event(
        &emitter,
        4,
        "turn_started",
        r#"{"turn":"turn-2","origin":"user"}"#,
    )
    .await?;
    wait_for_len(
        gateway_commands,
        5,
        "outbound Workflow did not start queued stream",
    )
    .await?;
    emit_session_event(&emitter, 5, "output_delta", r#"{"text":"second reply"}"#).await?;
    wait_for_len(
        gateway_commands,
        6,
        "outbound Workflow did not forward queued output",
    )
    .await?;
    emit_session_event(&emitter, 6, "turn_ended", r#"{"turn":"turn-2"}"#).await?;
    wait_for_len(
        gateway_commands,
        7,
        "outbound Workflow did not finish queued stream",
    )
    .await?;

    emit_session_event(&emitter, 7, "closed", r#"{"reason":"closed"}"#).await?;
    emitter
        .emit::<GatewayMessageReceived>(
            r#"{"route":{"channel":"imessage","conversation_id":"chat-1","thread_id":"thread-1"},"message_id":"message-3","text":"stale session"}"#,
        )
        .await
        .map_err(ComponentError::lifecycle)?;
    wait_for_len(
        gateway_commands,
        8,
        "stale session error did not reach Gateway",
    )
    .await?;
    Ok(())
}

async fn emit_session_event(
    emitter: &EventEmitter<FRAME_SIZE>,
    sequence: u64,
    event_type: &str,
    payload: &str,
) -> Result<(), ComponentError> {
    let document = format!(
        r#"{{"session":"session-1","sequence":{sequence},"type":"{event_type}","payload":{payload}}}"#,
    );
    emitter
        .emit::<SessionEvent>(&document)
        .await
        .map_err(ComponentError::lifecycle)
}

async fn wait_for_len(
    values: &RefCell<Vec<String>>,
    expected: usize,
    message: &'static str,
) -> Result<(), ComponentError> {
    for _iteration in 0..128 {
        if values.borrow().len() >= expected {
            return Ok(());
        }
        futures_lite::future::yield_now().await;
    }
    Err(ComponentError::lifecycle(WorkflowTestError(message)))
}

#[test]
fn builtin_workflows_restore_before_plugins_and_bridge_both_directions() {
    futures_lite::future::block_on(async {
        install_global_memory_vfs().await.expect("install test VFS");
        create_dir_all("/system").await.expect("create system dir");
        write("/system/workflows.json", WORKFLOWS.as_bytes())
            .await
            .expect("install builtin Workflows");

        let partition = memory_partition(64 * 1024).await.expect("partition");
        let mut manager = PluginManager::open(partition).await.expect("manager");
        let lanes = Box::leak(Box::new(RpcLaneStorage::<12, FRAME_SIZE, 12>::new()));
        let mut router = EventRouter::new(lanes).await.expect("restore Workflows");
        assert_eq!(router.workflow_definitions().len(), 2);

        let stack = never_embassy_stack();
        let mut plugin_context = PluginContext::new(stack, ClientFactory::plaintext(stack));
        manager
            .register(&mut router, ImessageBridgePlugin::new(&mut plugin_context))
            .expect("register bridge Plugin");

        let appended = Rc::new(RefCell::new(Vec::new()));
        let gateway_commands = Rc::new(RefCell::new(Vec::new()));
        let open_calls = Rc::new(Cell::new(0));
        let completed = Rc::new(Cell::new(false));
        let failure = Rc::new(RefCell::new(None));
        router
            .load(Box::new(WorkflowDriver {
                appended: Rc::clone(&appended),
                gateway_commands: Rc::clone(&gateway_commands),
                open_calls: Rc::clone(&open_calls),
                completed: Rc::clone(&completed),
                failure: Rc::clone(&failure),
            }))
            .expect("load Workflow driver");

        poll_fn(|context| {
            if let Poll::Ready(result) = Pin::new(&mut router).poll(context) {
                return Poll::Ready(result.map_err(|error| error.to_string()));
            }
            if completed.get() {
                Poll::Ready(Ok(()))
            } else {
                Poll::Pending
            }
        })
        .await
        .expect("drive builtin Workflows");

        assert_eq!(failure.borrow().as_deref(), None);
        assert_eq!(
            appended.borrow().as_slice(),
            &[
                r#"{"text":"hello agent","session":"session-1"}"#,
                r#"{"text":"queued message","session":"session-1"}"#,
            ]
        );
        assert_eq!(gateway_commands.borrow().len(), 8);
        for (index, event_type) in [
            "turn_started",
            "reasoning_delta",
            "output_delta",
            "turn_ended",
        ]
        .iter()
        .enumerate()
        {
            let command = &gateway_commands.borrow()[index];
            assert!(command.contains(&format!(r#""type":"{event_type}""#)));
            assert!(command.contains(r#""route":{"channel":"imessage","conversation_id":"chat-1","thread_id":"thread-1"}"#));
            assert!(command.contains(r#""reply_to":"message-1""#));
        }
        for (offset, event_type) in ["turn_started", "output_delta", "turn_ended"]
            .iter()
            .enumerate()
        {
            let command = &gateway_commands.borrow()[offset + 4];
            assert!(command.contains(&format!(r#""type":"{event_type}""#)));
            assert!(command.contains(r#""route":{"channel":"imessage","conversation_id":"chat-1","thread_id":"thread-1"}"#));
            assert!(command.contains(r#""reply_to":"message-2""#));
        }
        let stale = &gateway_commands.borrow()[7];
        assert!(stale.contains(r#""type":"stream_error""#));
        assert!(stale.contains(r#""reply_to":"message-3""#));
        assert!(stale.contains(r#""payload":{"session":"session-1","error":"worker_stopped"}"#));
        assert_eq!(appended.borrow().len(), 2);
    });
}
