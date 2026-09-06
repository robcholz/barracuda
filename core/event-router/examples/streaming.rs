//! Usage: application-level samples travel as one JSON array through a Workflow.

use std::cell::{Cell, RefCell};
use std::future::{pending, poll_fn, Future};
use std::pin::Pin;
use std::rc::Rc;
use std::task::Poll;

use barracuda_event_router::{
    Component, ComponentError, ComponentFuture, ComponentResult, Event, EventEmitter, EventRouter,
    JsonRef, JsonRpcSchema, JsonSchema, JsonWriter, RegisterContext, RpcLaneStorage, RunContext,
    UnregisterContext, WorkflowClient,
};
use barracuda_platform_test::install_global_memory_vfs;
use serde::Deserialize;
use static_cell::ConstStaticCell;

const FRAME_SIZE: usize = 256;
const SAMPLES_SCHEMA: JsonSchema = barracuda_rpc::json_schema_inline!(
    r#"{"type":"object","properties":{"samples":{"type":"array","items":{"type":"integer"}}},"required":["samples"],"additionalProperties":false}"#,
);
const EMPTY_SCHEMA: JsonSchema = barracuda_rpc::json_schema_inline!(
    r#"{"type":"object","properties":{},"additionalProperties":false}"#
);

static RPC_LANES: ConstStaticCell<RpcLaneStorage<4, FRAME_SIZE, 4>> =
    ConstStaticCell::new(RpcLaneStorage::new());

const WORKFLOW_JSON: &str = r#"{
    "id": "sample-collector",
    "match": { "event": "sensor.samples" },
    "steps": [{ "call": "example.collect" }]
}"#;

struct SensorSamples;

impl Event for SensorSamples {
    const ID: &'static str = "sensor.samples";
}

struct Collect;

impl JsonRpcSchema for Collect {
    const ADDRESS: &'static str = "example.collect";
    const REQUEST_SCHEMA: JsonSchema = SAMPLES_SCHEMA;
    const RESPONSE_SCHEMA: JsonSchema = EMPTY_SCHEMA;
    const MAX_REQUEST_BYTES: usize = 64;
    const MAX_RESPONSE_BYTES: usize = 2;
}

#[derive(Deserialize)]
struct Samples {
    samples: Vec<u8>,
}

#[derive(Default)]
struct StreamState {
    emitted: Cell<bool>,
    collected: Rc<RefCell<Vec<u8>>>,
}

struct StreamingDemo {
    state: Rc<StreamState>,
}

impl Component<FRAME_SIZE> for StreamingDemo {
    fn name(&self) -> &'static str {
        "streaming-demo"
    }

    fn register(&mut self, context: &mut RegisterContext<'_, FRAME_SIZE>) -> ComponentResult<()> {
        let collected = Rc::clone(&self.state.collected);
        context.register_json::<Collect, _>(
            "*",
            move |_context, request: JsonRef, response: JsonWriter| {
                let collected = Rc::clone(&collected);
                async move {
                    collected
                        .borrow_mut()
                        .extend(request.deserialize::<Samples>()?.samples);
                    response.write("{}").await
                }
            },
        )
    }

    fn run<'a>(&'a mut self, context: RunContext<FRAME_SIZE>) -> ComponentFuture<'a> {
        Box::pin(async move {
            let client = context.rpc().clone();
            WorkflowClient::<FRAME_SIZE>::new(client.clone())
                .load(WORKFLOW_JSON)
                .await
                .map_err(ComponentError::lifecycle)?;
            EventEmitter::<FRAME_SIZE>::new(client)
                .emit::<SensorSamples>(r#"{"samples":[10,11,12]}"#)
                .await
                .map_err(ComponentError::lifecycle)?;
            self.state.emitted.set(true);
            pending().await
        })
    }

    fn unregister(&mut self, _context: &mut UnregisterContext<'_>) -> ComponentResult<()> {
        Ok(())
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn core::error::Error>> {
    let state = Rc::new(StreamState::default());
    install_global_memory_vfs().await?;
    let mut event_router = EventRouter::new(RPC_LANES.take()).await?;
    let demo = event_router.load(Box::new(StreamingDemo {
        state: Rc::clone(&state),
    }))?;

    poll_fn(|context| {
        if let Poll::Ready(result) = Pin::new(&mut event_router).poll(context) {
            return Poll::Ready(result);
        }
        if state.emitted.get() && event_router.workflow_info().completed_count == 1 {
            Poll::Ready(Ok(()))
        } else {
            Poll::Pending
        }
    })
    .await?;

    assert_eq!(state.collected.borrow().as_slice(), [10, 11, 12]);
    assert_eq!(event_router.workflow_info().failed_count, 0);
    event_router.unload(demo)?;
    println!("Application-level sample array preserved order through the Workflow");
    Ok(())
}
