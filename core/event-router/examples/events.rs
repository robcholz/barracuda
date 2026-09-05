//! Usage: emit a JSON Event and inspect immutable Workflow state.

use std::cell::Cell;
use std::future::{pending, poll_fn, Future};
use std::pin::Pin;
use std::rc::Rc;
use std::task::Poll;

use barracuda_event_router::{
    Component, ComponentError, ComponentFuture, ComponentResult, Event, EventEmitter, EventRouter,
    JsonRef, JsonRpcSchema, JsonSchema, JsonWriter, RegisterContext, RpcLaneStorage, RunContext,
    UnregisterContext, WorkflowClient, WorkflowInfo,
};
use barracuda_platform_test::install_global_memory_vfs;
use serde::Deserialize;
use static_cell::ConstStaticCell;

const FRAME_SIZE: usize = 256;
const MESSAGE_SCHEMA: JsonSchema = JsonSchema::new(
    r#"{"type":"object","properties":{"bytes":{"type":"array","items":{"type":"integer"},"minItems":4,"maxItems":4}},"required":["bytes"],"additionalProperties":false}"#,
);
const EMPTY_SCHEMA: JsonSchema =
    JsonSchema::new(r#"{"type":"object","properties":{},"additionalProperties":false}"#);

static RPC_LANES: ConstStaticCell<RpcLaneStorage<3, FRAME_SIZE, 3>> =
    ConstStaticCell::new(RpcLaneStorage::new());

const WORKFLOW_JSON: &str = r#"{
    "id": "gateway-message-recorder",
    "match": { "event": "gateway.message.received" },
    "steps": [{ "call": "example.record-message" }]
}"#;

struct GatewayMessageReceived;

impl Event for GatewayMessageReceived {
    const ID: &'static str = "gateway.message.received";
}

struct RecordMessage;

impl JsonRpcSchema for RecordMessage {
    const ADDRESS: &'static str = "example.record-message";
    const REQUEST_SCHEMA: JsonSchema = MESSAGE_SCHEMA;
    const RESPONSE_SCHEMA: JsonSchema = EMPTY_SCHEMA;
    const MAX_REQUEST_BYTES: usize = 32;
    const MAX_RESPONSE_BYTES: usize = 2;
}

#[derive(Deserialize)]
struct Message {
    bytes: [u8; 4],
}

#[derive(Default)]
struct GatewayState {
    event_accepted: Cell<bool>,
    recorded: Cell<Option<[u8; 4]>>,
    unregistered: Cell<bool>,
}

struct Gateway {
    state: Rc<GatewayState>,
}

impl Component<FRAME_SIZE> for Gateway {
    fn name(&self) -> &'static str {
        "gateway"
    }

    fn register(&mut self, context: &mut RegisterContext<'_, FRAME_SIZE>) -> ComponentResult<()> {
        let state = Rc::clone(&self.state);
        context.register_json::<RecordMessage, _>(
            "*",
            move |_context, request: JsonRef, response: JsonWriter| {
                let state = Rc::clone(&state);
                async move {
                    state
                        .recorded
                        .set(Some(request.deserialize::<Message>()?.bytes));
                    response.write("{}").await
                }
            },
        )
    }

    fn run<'a>(&'a mut self, context: RunContext<FRAME_SIZE>) -> ComponentFuture<'a> {
        Box::pin(async move {
            WorkflowClient::<FRAME_SIZE>::new(context.rpc().clone())
                .load(WORKFLOW_JSON)
                .await
                .map_err(ComponentError::lifecycle)?;
            EventEmitter::<FRAME_SIZE>::new(context.rpc().clone())
                .emit::<GatewayMessageReceived>(r#"{"bytes":[1,2,3,4]}"#)
                .await
                .map_err(ComponentError::lifecycle)?;
            self.state.event_accepted.set(true);
            pending().await
        })
    }

    fn unregister(&mut self, _context: &mut UnregisterContext<'_>) -> ComponentResult<()> {
        self.state.unregistered.set(true);
        Ok(())
    }
}

fn assert_idle(info: &WorkflowInfo) {
    assert_eq!(info.completed_count, 0);
    assert_eq!(info.failed_count, 0);
    assert_eq!(info.cancelled_count, 0);
    assert!(info.last_failure.is_none());
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn core::error::Error>> {
    let state = Rc::new(GatewayState::default());
    install_global_memory_vfs().await?;
    let mut event_router = EventRouter::new(RPC_LANES.take()).await?;

    assert!(event_router.workflow_definitions().is_empty());
    assert_idle(&event_router.workflow_info());
    let gateway = event_router.load(Box::new(Gateway {
        state: Rc::clone(&state),
    }))?;

    poll_fn(|context| {
        if let Poll::Ready(result) = Pin::new(&mut event_router).poll(context) {
            return Poll::Ready(result);
        }
        if state.event_accepted.get()
            && state.recorded.get().is_some()
            && event_router.workflow_info().completed_count == 1
        {
            Poll::Ready(Ok(()))
        } else {
            Poll::Pending
        }
    })
    .await?;

    assert_eq!(state.recorded.get(), Some([1, 2, 3, 4]));
    assert_eq!(event_router.workflow_definitions().len(), 1);
    assert_eq!(event_router.workflow_info().completed_count, 1);
    event_router.unload(gateway)?;
    assert!(state.unregistered.get());
    println!("Workflow loaded through JSON RPC; a matching JSON Event reached its JSON RPC step");
    Ok(())
}
