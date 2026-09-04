//! Usage: a Workflow step that returns its Method error is recorded in the
//! immutable `WorkflowInfo` snapshot.

use std::cell::Cell;
use std::future::{pending, poll_fn, Future};
use std::pin::Pin;
use std::rc::Rc;
use std::task::Poll;

use barracuda_event_router::{
    Component, ComponentError, ComponentFuture, ComponentResult, Event, EventEmitter, EventRouter,
    RegisterContext, RpcFrame, RpcLaneStorage, RpcMethod, RunContext, Unary, UnregisterContext,
    WorkflowClient,
};
use barracuda_platform_test::install_global_memory_vfs;
use static_cell::ConstStaticCell;

const FRAME_SIZE: usize = 64;

static RPC_LANES: ConstStaticCell<RpcLaneStorage<4, FRAME_SIZE, 4>> =
    ConstStaticCell::new(RpcLaneStorage::new());

const WORKFLOW_JSON: &str = r#"{
    "id": "always-fails",
    "match": { "event": "example.fail" },
    "steps": [
        { "call": "example.fail" }
    ]
}"#;

struct FailEvent;

impl Event for FailEvent {
    const ID: &'static str = "example.fail";
    type Message = [u8; 1];
    type Input = Unary;
}

struct Fail;

impl RpcMethod for Fail {
    const ADDRESS: &'static str = "example.fail";
    type Request = [u8; 1];
    type Response = ();
    type Error = ();
    type Input = Unary;
    type Output = Unary;
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
        context.register_rpc::<Fail, _>(
            "system",
            |_context, _request: RpcFrame<[u8; 1]>| async move { Ok(Err(())) },
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
                .emit::<FailEvent>([1])
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
    assert!(info.last_failure.is_some());
    if let Some(failure) = &info.last_failure {
        assert_eq!(failure.workflow_id().as_str(), "always-fails");
        println!("Workflow failed as expected: {}", failure.error());
    }

    event_router.unload(demo)?;
    Ok(())
}
