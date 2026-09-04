//! Usage: a streaming Event fans into a streaming Workflow step in order.

use std::cell::{Cell, RefCell};
use std::future::{pending, poll_fn, Future};
use std::pin::Pin;
use std::rc::Rc;
use std::task::Poll;

use barracuda_event_router::{
    Component, ComponentError, ComponentFuture, ComponentResult, Event, EventEmitter, EventRouter,
    RegisterContext, RpcFrame, RpcLaneStorage, RpcMethod, RpcStream, RunContext, Streaming, Unary,
    UnregisterContext, WorkflowClient,
};
use barracuda_platform_test::install_global_memory_vfs;
use futures_util::stream;
use static_cell::ConstStaticCell;

const FRAME_SIZE: usize = 64;

static RPC_LANES: ConstStaticCell<RpcLaneStorage<4, FRAME_SIZE, 4>> =
    ConstStaticCell::new(RpcLaneStorage::new());

const WORKFLOW_JSON: &str = r#"{
    "id": "streaming-event-collector",
    "match": { "event": "sensor.samples" },
    "steps": [
        { "call": "example.collect" }
    ]
}"#;

struct SensorSamples;

impl Event for SensorSamples {
    const ID: &'static str = "sensor.samples";
    type Message = [u8; 1];
    type Input = Streaming;
}

struct Collect;

impl RpcMethod for Collect {
    const ADDRESS: &'static str = "example.collect";
    type Request = [u8; 1];
    type Response = ();
    type Error = ();
    type Input = Streaming;
    type Output = Unary;
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
    fn register(&mut self, context: &mut RegisterContext<'_, FRAME_SIZE>) -> ComponentResult<()> {
        let collected = Rc::clone(&self.state.collected);
        context.register_rpc::<Collect, _>(
            "system",
            move |_context, mut requests: RpcStream<RpcFrame<[u8; 1]>>| {
                let collected = Rc::clone(&collected);
                async move {
                    while let Some(request) = requests.next().await {
                        let [value] = *request?.view()?;
                        collected.borrow_mut().push(value);
                    }
                    Ok(Ok(()))
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
            let samples = RpcStream::new(stream::iter([Ok([10]), Ok([11]), Ok([12])]));
            EventEmitter::<FRAME_SIZE>::new(client)
                .emit::<SensorSamples>(samples)
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
    println!("Streaming Event preserved message order through the Workflow step");
    Ok(())
}
