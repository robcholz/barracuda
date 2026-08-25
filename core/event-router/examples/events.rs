//! Usage: emit a typed Event and inspect immutable Workflow state.

use std::cell::Cell;
use std::future::{pending, poll_fn, Future};
use std::pin::Pin;
use std::rc::Rc;
use std::task::Poll;

use barracuda_event_router::{
    Component, ComponentError, ComponentFuture, ComponentResult, Event, EventEmitter, EventRouter,
    RegisterContext, RpcFrame, RpcLaneStorage, RpcMethod, RunContext, Unary, UnregisterContext,
    WorkflowClient, WorkflowInfo,
};
use barracuda_platform_test::install_global_memory_vfs;
use static_cell::ConstStaticCell;

const FRAME_SIZE: usize = 256;

static RPC_LANES: ConstStaticCell<RpcLaneStorage<3, FRAME_SIZE, 3>> =
    ConstStaticCell::new(RpcLaneStorage::new());

const WORKFLOW_JSON: &str = r#"{
    "id": "gateway-message-recorder",
    "match": { "event": "gateway.message.received" },
    "steps": [
        { "call": "example.record-message" }
    ]
}"#;

struct GatewayMessageReceived;

impl Event for GatewayMessageReceived {
    const ID: &'static str = "gateway.message.received";
    type Message = [u8; 4];
    type Input = Unary;
}

#[derive(Default)]
struct GatewayState {
    event_accepted: Cell<bool>,
    recorded: Cell<Option<[u8; 4]>>,
    unregistered: Cell<bool>,
}

struct RecordMessage;

impl RpcMethod for RecordMessage {
    const ADDRESS: &'static str = "example.record-message";
    type Request = [u8; 4];
    type Response = ();
    type Error = ();
    type Input = Unary;
    type Output = Unary;
}

struct Gateway {
    state: Rc<GatewayState>,
}

impl Component<FRAME_SIZE> for Gateway {
    fn register(&mut self, context: &mut RegisterContext<'_, FRAME_SIZE>) -> ComponentResult<()> {
        let state = Rc::clone(&self.state);
        context.register_rpc::<RecordMessage, _>(move |_context, request: RpcFrame<[u8; 4]>| {
            let state = Rc::clone(&state);
            async move {
                state.recorded.set(Some(*request.view()?));
                Ok(Ok(()))
            }
        })
    }

    fn run<'a>(&'a mut self, context: RunContext<FRAME_SIZE>) -> ComponentFuture<'a> {
        Box::pin(async move {
            WorkflowClient::<FRAME_SIZE>::new(context.rpc().clone())
                .load(WORKFLOW_JSON)
                .await
                .map_err(ComponentError::lifecycle)?;
            EventEmitter::<FRAME_SIZE>::new(context.rpc().clone())
                .emit::<GatewayMessageReceived>([1, 2, 3, 4])
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
    let info = event_router.workflow_info();
    assert_eq!(info.completed_count, 1);
    assert_eq!(info.failed_count, 0);

    event_router.unload(gateway)?;
    assert!(state.unregistered.get());
    println!("Workflow loaded from JSON; matching Event reached its RPC step");
    Ok(())
}
