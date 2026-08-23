#![allow(missing_docs)]
#![allow(clippy::expect_used)]

use std::cell::{Cell, RefCell};
use std::future::{pending, poll_fn};
use std::pin::Pin;
use std::rc::Rc;
use std::task::Poll;

use barracuda_event_router::{
    Component, ComponentError, ComponentFuture, ComponentResult, EventRouter, MemFs,
    RegisterContext, RpcAddress, RpcError, RpcFrame, RpcLaneStorage, RpcMethod, RunContext, Unary,
    UnregisterContext, WorkflowClient,
};
use barracuda_scheduler_component::event::Triggered;
use barracuda_scheduler_component::{SchedulerComponent, SchedulerConfig};
use barracuda_time_component::{
    SyncSample, TimeComponent, TimeConfig, TimeSource, TimeSourceFuture,
};
use embassy_time::Instant;
use serde_json::json;

const FRAME_SIZE: usize = 256;
const WORKFLOW: &str = r#"{
    "id":"scheduler-test",
    "match":{"event":"scheduler.triggered","topic":"typed-time-flow"},
    "steps":[{"call":"scheduler-test.record"}]
}"#;

struct ImmediateNetworkTime;

impl TimeSource for ImmediateNetworkTime {
    fn synchronize(&mut self) -> TimeSourceFuture<'_> {
        Box::pin(async { Ok(SyncSample::new(1_800_000_000_000, Instant::now())) })
    }
}

struct Record;

impl RpcMethod for Record {
    const ADDRESS: &'static str = "scheduler-test.record";
    type Request = Triggered;
    type Response = ();
    type Error = ();
    type Input = Unary;
    type Output = Unary;
}

struct TestDriver {
    recorded: Rc<RefCell<Option<Triggered>>>,
    scheduled: Rc<Cell<bool>>,
}

impl Component<FRAME_SIZE> for TestDriver {
    fn register(&mut self, context: &mut RegisterContext<'_, FRAME_SIZE>) -> ComponentResult<()> {
        let recorded = Rc::clone(&self.recorded);
        context.register_rpc::<Record, _>(move |_context, request: RpcFrame<Triggered>| {
            let recorded = Rc::clone(&recorded);
            async move {
                recorded.replace(Some(*request.view()?));
                Ok(Ok(()))
            }
        })
    }

    fn run<'a>(&'a mut self, context: RunContext<FRAME_SIZE>) -> ComponentFuture<'a> {
        Box::pin(async move {
            let client = context.rpc().clone();
            WorkflowClient::<FRAME_SIZE>::new(client.clone())
                .load(WORKFLOW)
                .await
                .map_err(ComponentError::lifecycle)?;
            let address = RpcAddress::try_from("scheduler.schedule").map_err(RpcError::from)?;
            client
                .call_json(
                    &address,
                    &json!({
                        "id":"typed-time-flow",
                        "trigger": {
                            "type":"once",
                            "at": {
                                "year":2027,
                                "month":1,
                                "day":15,
                                "hour":8,
                                "minute":0,
                                "second":0
                            }
                        }
                    }),
                )
                .await?;
            self.scheduled.set(true);
            pending().await
        })
    }

    fn unregister(&mut self, _context: &mut UnregisterContext<'_>) -> ComponentResult<()> {
        Ok(())
    }
}

#[test]
fn scheduler_calls_typed_time_rpc_and_routes_trigger_event() {
    let lanes = Box::leak(Box::new(RpcLaneStorage::<8, FRAME_SIZE, 8>::new()));
    let filesystem = Box::leak(Box::new(MemFs::new()));
    let mut router = EventRouter::new(lanes, filesystem, "scheduler-component-flow")
        .expect("create Event Router");
    router
        .load(Box::new(TimeComponent::new(
            ImmediateNetworkTime,
            TimeConfig::new(10, 60_000, 120_000),
        )))
        .expect("load time Component");
    router
        .load(Box::new(SchedulerComponent::new(SchedulerConfig::new(
            8, 10,
        ))))
        .expect("load scheduler Component");
    let recorded = Rc::new(RefCell::new(None));
    let scheduled = Rc::new(Cell::new(false));
    router
        .load(Box::new(TestDriver {
            recorded: Rc::clone(&recorded),
            scheduled: Rc::clone(&scheduled),
        }))
        .expect("load test driver");

    futures_lite::future::block_on(poll_fn(|context| {
        if let Poll::Ready(result) = Pin::new(&mut router).poll(context) {
            return Poll::Ready(result);
        }
        if scheduled.get() && recorded.borrow().is_some() {
            Poll::Ready(Ok(()))
        } else {
            Poll::Pending
        }
    }))
    .expect("drive scheduler flow");

    let event = recorded.borrow().expect("trigger event");
    assert_eq!(event.id().as_str(), "typed-time-flow");
    assert_eq!(event.run_number(), 1);
}
