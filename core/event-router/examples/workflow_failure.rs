//! Usage: a failed JSON RPC step is recorded in immutable Workflow state.

use std::cell::Cell;
use std::future::{pending, poll_fn, Future};
use std::pin::Pin;
use std::rc::Rc;
use std::task::Poll;

use barracuda_event_router::{
    Component, ComponentError, ComponentFuture, ComponentResult, Event, EventEmitter, EventRouter,
    JsonRpcSchema, JsonSchema, RegisterContext, RpcError, RpcLaneStorage, RunContext,
    UnregisterContext, WorkflowClient,
};
use barracuda_platform_test::install_global_memory_vfs;
use static_cell::ConstStaticCell;

const FRAME_SIZE: usize = 256;
const EMPTY_SCHEMA: JsonSchema = barracuda_rpc::json_schema_inline!(
    r#"{"type":"object","properties":{},"additionalProperties":false}"#
);

static RPC_LANES: ConstStaticCell<RpcLaneStorage<4, FRAME_SIZE, 4>> =
    ConstStaticCell::new(RpcLaneStorage::new());

const WORKFLOW_JSON: &str = r#"{
    "id": "always-fails",
    "match": { "event": "example.fail" },
    "steps": [{ "call": "example.fail" }]
}"#;

struct FailEvent;

impl Event for FailEvent {
    const ID: &'static str = "example.fail";
}

struct Fail;

impl JsonRpcSchema for Fail {
    const ADDRESS: &'static str = "example.fail";
    const REQUEST_SCHEMA: JsonSchema = EMPTY_SCHEMA;
    const RESPONSE_SCHEMA: JsonSchema = EMPTY_SCHEMA;
    const MAX_REQUEST_BYTES: usize = 2;
    const MAX_RESPONSE_BYTES: usize = 2;
}

#[derive(Default)]
struct FailureState {
    emitted: Cell<bool>,
}

struct FailureDemo {
    state: Rc<FailureState>,
}

impl Component<FRAME_SIZE> for FailureDemo {
    fn register(&mut self, context: &mut RegisterContext<'_, FRAME_SIZE>) -> ComponentResult<()> {
        context.register_json::<Fail, _>("*", |_context, _request, _response| async move {
            Err(RpcError::InvalidFrameState)
        })
    }

    fn run<'a>(&'a mut self, context: RunContext<FRAME_SIZE>) -> ComponentFuture<'a> {
        Box::pin(async move {
            let client = context.rpc().clone();
            WorkflowClient::<FRAME_SIZE>::new(client.clone())
                .load(WORKFLOW_JSON)
                .await
                .map_err(ComponentError::lifecycle)?;
            EventEmitter::<FRAME_SIZE>::new(client)
                .emit::<FailEvent>("{}")
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
    let state = Rc::new(FailureState::default());
    install_global_memory_vfs().await?;
    let mut event_router = EventRouter::new(RPC_LANES.take()).await?;
    let demo = event_router.load(Box::new(FailureDemo {
        state: Rc::clone(&state),
    }))?;

    poll_fn(|context| {
        if let Poll::Ready(result) = Pin::new(&mut event_router).poll(context) {
            return Poll::Ready(result);
        }
        if state.emitted.get() && event_router.workflow_info().failed_count == 1 {
            Poll::Ready(Ok(()))
        } else {
            Poll::Pending
        }
    })
    .await?;

    let info = event_router.workflow_info();
    assert_eq!(info.completed_count, 0);
    assert_eq!(info.failed_count, 1);
    assert_eq!(
        info.last_failure
            .as_ref()
            .map(|failure| failure.workflow_id().as_str()),
        Some("always-fails")
    );
    event_router.unload(demo)?;
    Ok(())
}
