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
const ANY_JSON: JsonSchema = JsonSchema::new("{}");
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
        context.register_json::<OpenSession, _>(
            "system",
            |_context, _request, response: JsonWriter| async move {
                response
                    .write(r#"{"session":"session-1","run":"run-1"}"#)
                    .await
            },
        )?;
        let appended = Rc::clone(&self.appended);
        context.register_json::<AppendSession, _>(
            "system",
            move |_context, request: JsonRef, response: JsonWriter| {
                let appended = Rc::clone(&appended);
                async move {
                    appended.borrow_mut().push(request.as_str()?.to_string());
                    response.write("{}").await
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

    emit_session_field(&emitter, 0, 0, "type", "turn_started", true, false).await?;
    wait_for_len(
        gateway_commands,
        1,
        "outbound Workflow did not start stream",
    )
    .await?;
    emit_session_field(&emitter, 1, 0, "type", "reasoning_delta", true, false).await?;
    emit_session_field(&emitter, 1, 1, "text", "private", true, true).await?;
    for _iteration in 0..8 {
        futures_lite::future::yield_now().await;
    }
    if gateway_commands.borrow().len() != 1 {
        return Err(ComponentError::lifecycle(WorkflowTestError(
            "reasoning text was forwarded",
        )));
    }
    emit_session_field(&emitter, 2, 0, "type", "output_delta", true, false).await?;
    emit_session_field(&emitter, 2, 1, "text", "hello user", true, true).await?;
    wait_for_len(
        gateway_commands,
        2,
        "outbound Workflow did not forward output",
    )
    .await?;
    emit_session_field(&emitter, 3, 0, "type", "turn_ended", true, false).await?;
    wait_for_len(
        gateway_commands,
        3,
        "outbound Workflow did not finish stream",
    )
    .await?;
    Ok(())
}

async fn emit_session_field(
    emitter: &EventEmitter<FRAME_SIZE>,
    sequence: u64,
    chunk_index: u64,
    field: &str,
    chunk: &str,
    field_complete: bool,
    event_complete: bool,
) -> Result<(), ComponentError> {
    let document = format!(
        r#"{{"session":"session-1","run":"run-1","sequence":{sequence},"chunk_index":{chunk_index},"field":"{field}","chunk":"{chunk}","field_complete":{field_complete},"event_complete":{event_complete},"terminal":null}}"#,
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
        let completed = Rc::new(Cell::new(false));
        let failure = Rc::new(RefCell::new(None));
        router
            .load(Box::new(WorkflowDriver {
                appended: Rc::clone(&appended),
                gateway_commands: Rc::clone(&gateway_commands),
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
            &[r#"{"text":"hello agent","session":"session-1"}"#]
        );
        assert_eq!(gateway_commands.borrow().len(), 3);
        assert!(gateway_commands.borrow()[0].contains(r#""action":"start""#));
        assert!(gateway_commands.borrow()[1].contains(r#""text":"hello user""#));
        assert!(gateway_commands.borrow()[2].contains(r#""action":"finish""#));
    });
}
