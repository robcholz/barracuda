//! Usage: a multi-step Workflow maps JSON fields between RPC calls.

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
use serde_json::json;
use static_cell::ConstStaticCell;

const FRAME_SIZE: usize = 512;
const SEED_SCHEMA: JsonSchema = barracuda_rpc::json_schema_inline!(
    r#"{"type":"object","properties":{"n":{"type":"integer"}},"required":["n"],"additionalProperties":false}"#,
);
const REPLY_SCHEMA: JsonSchema = barracuda_rpc::json_schema_inline!(
    r#"{"type":"object","properties":{"token":{"type":"integer"}},"required":["token"],"additionalProperties":false}"#,
);
const DELIVER_SCHEMA: JsonSchema = barracuda_rpc::json_schema_inline!(
    r#"{"type":"object","properties":{"token":{"type":"integer"},"extra":{"type":"integer"}},"required":["token","extra"],"additionalProperties":false}"#,
);
const EMPTY_SCHEMA: JsonSchema = barracuda_rpc::json_schema_inline!(
    r#"{"type":"object","properties":{},"additionalProperties":false}"#
);

static RPC_LANES: ConstStaticCell<RpcLaneStorage<4, FRAME_SIZE, 4>> =
    ConstStaticCell::new(RpcLaneStorage::new());

const WORKFLOW_JSON: &str = r#"{
    "id": "seed-to-deliver",
    "match": { "event": "example.seed" },
    "steps": [
        { "call": "example.produce" },
        { "call": "example.consume", "arguments": { "extra": 5, "token": "$previous.output.token" } }
    ]
}"#;

#[derive(Deserialize)]
struct Seed {
    n: u32,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
struct Deliver {
    token: u32,
    extra: u32,
}

struct SeedEvent;

impl Event for SeedEvent {
    const ID: &'static str = "example.seed";
}

struct Produce;

impl JsonRpcSchema for Produce {
    const ADDRESS: &'static str = "example.produce";
    const REQUEST_SCHEMA: JsonSchema = SEED_SCHEMA;
    const RESPONSE_SCHEMA: JsonSchema = REPLY_SCHEMA;
    const MAX_REQUEST_BYTES: usize = 32;
    const MAX_RESPONSE_BYTES: usize = 32;
}

struct Consume;

impl JsonRpcSchema for Consume {
    const ADDRESS: &'static str = "example.consume";
    const REQUEST_SCHEMA: JsonSchema = DELIVER_SCHEMA;
    const RESPONSE_SCHEMA: JsonSchema = EMPTY_SCHEMA;
    const MAX_REQUEST_BYTES: usize = 64;
    const MAX_RESPONSE_BYTES: usize = 2;
}

#[derive(Default)]
struct WorkflowState {
    emitted: Cell<bool>,
    recorded: Rc<RefCell<Option<Deliver>>>,
}

struct WorkflowDemo {
    state: Rc<WorkflowState>,
}

impl Component<FRAME_SIZE> for WorkflowDemo {
    fn register(&mut self, context: &mut RegisterContext<'_, FRAME_SIZE>) -> ComponentResult<()> {
        context.register_json::<Produce, _>(
            "*",
            |_context, request: JsonRef, response: JsonWriter| async move {
                let seed = request.deserialize::<Seed>()?;
                response.write(&json!({"token": seed.n})).await
            },
        )?;
        let recorded = Rc::clone(&self.state.recorded);
        context.register_json::<Consume, _>(
            "*",
            move |_context, request: JsonRef, response: JsonWriter| {
                let recorded = Rc::clone(&recorded);
                async move {
                    recorded.replace(Some(request.deserialize::<Deliver>()?));
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
                .emit::<SeedEvent>(r#"{"n":41}"#)
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
    let state = Rc::new(WorkflowState::default());
    install_global_memory_vfs().await?;
    let mut event_router = EventRouter::new(RPC_LANES.take()).await?;
    let demo = event_router.load(Box::new(WorkflowDemo {
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

    assert_eq!(
        state.recorded.borrow().as_ref(),
        Some(&Deliver {
            token: 41,
            extra: 5
        })
    );
    assert_eq!(event_router.workflow_info().failed_count, 0);
    event_router.unload(demo)?;
    println!("Workflow mapped `$previous.output.token` into the next JSON request");
    Ok(())
}
