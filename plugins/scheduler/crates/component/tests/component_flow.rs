#![allow(missing_docs)]
#![allow(clippy::expect_used)]

use std::cell::{Cell, RefCell};
use std::future::{pending, poll_fn};
use std::pin::Pin;
use std::rc::Rc;
use std::task::Poll;

use barracuda_event_router::{
    Component, ComponentError, ComponentFuture, ComponentResult, EventRouter, RegisterContext,
    RpcAddress, RpcError, RpcFrame, RpcLaneStorage, RpcMethod, RunContext, Unary,
    UnregisterContext, WorkflowClient,
};
use barracuda_platform_test::install_global_memory_vfs;
use barracuda_scheduler_component::cancel::{Cancel, CancelRequest};
use barracuda_scheduler_component::event::Triggered;
use barracuda_scheduler_component::schedule::{
    Schedule, ScheduleError, ScheduleRequest, Trigger, TriggerAt,
};
use barracuda_scheduler_component::{ScheduleId, SchedulerComponent, SchedulerConfig};
use barracuda_time_component::{SyncSample, TimeComponent, TimeConfig};
use embassy_time::Instant;
use serde_json::json;

const FRAME_SIZE: usize = 256;
const WORKFLOW: &str = r#"{
    "id":"scheduler-test",
    "match":{"event":"scheduler.triggered","topic":"typed-time-flow"},
    "steps":[{"call":"scheduler-test.record"}]
}"#;

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
    futures_lite::future::block_on(install_global_memory_vfs()).expect("install global test VFS");
    let lanes = Box::leak(Box::new(RpcLaneStorage::<8, FRAME_SIZE, 8>::new()));
    let mut router =
        futures_lite::future::block_on(EventRouter::new(lanes)).expect("create Event Router");
    let time = TimeComponent::new(TimeConfig::new(10, 60_000, 120_000));
    time.shared_state()
        .borrow_mut()
        .synchronize(SyncSample::new(1_800_000_000_000, Instant::now()));
    router.load(Box::new(time)).expect("load time Component");
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

struct MutationDriver {
    done: Rc<Cell<bool>>,
}

impl Component<FRAME_SIZE> for MutationDriver {
    fn register(&mut self, _context: &mut RegisterContext<'_, FRAME_SIZE>) -> ComponentResult<()> {
        Ok(())
    }

    fn run<'a>(&'a mut self, context: RunContext<FRAME_SIZE>) -> ComponentFuture<'a> {
        Box::pin(async move {
            let client = context.rpc();
            let first = ScheduleId::new("first").map_err(ComponentError::lifecycle)?;
            let second = ScheduleId::new("second").map_err(ComponentError::lifecycle)?;
            let future = TriggerAt::new(2027, 1, 15, 8, 1, 0);

            let accepted = client
                .call::<Schedule>(ScheduleRequest::new(first, Trigger::once(future)))?
                .await?;
            let accepted = match accepted {
                Ok(response) => response,
                Err(_error) => return Err(ComponentError::lifecycle(RpcError::InvalidFrameState)),
            };
            assert_eq!(accepted.view()?.id(), first);

            let duplicate = client
                .call::<Schedule>(ScheduleRequest::new(first, Trigger::once(future)))?
                .await?;
            assert_eq!(
                *duplicate.expect_err("duplicate id is rejected").view()?,
                ScheduleError::DuplicateId
            );

            let capacity = client
                .call::<Schedule>(ScheduleRequest::new(second, Trigger::once(future)))?
                .await?;
            assert_eq!(
                *capacity.expect_err("capacity is enforced").view()?,
                ScheduleError::CapacityExceeded
            );

            let past = client
                .call::<Schedule>(ScheduleRequest::new(
                    second,
                    Trigger::once(TriggerAt::new(2027, 1, 15, 7, 59, 59)),
                ))?
                .await?;
            assert_eq!(
                *past.expect_err("past trigger is rejected").view()?,
                ScheduleError::TriggerInPast
            );

            let invalid = client
                .call::<Schedule>(ScheduleRequest::new(
                    second,
                    Trigger::interval(future, 0, 2),
                ))?
                .await?;
            assert_eq!(
                *invalid.expect_err("invalid interval is rejected").view()?,
                ScheduleError::InvalidSchedule
            );

            let missing = client.call::<Cancel>(CancelRequest::new(second))?.await?;
            assert_eq!(
                *missing.expect_err("missing schedule is rejected").view()?,
                ScheduleError::NotFound
            );

            let cancelled = client.call::<Cancel>(CancelRequest::new(first))?.await?;
            let cancelled = match cancelled {
                Ok(response) => response,
                Err(_error) => return Err(ComponentError::lifecycle(RpcError::InvalidFrameState)),
            };
            assert_eq!(cancelled.view()?.id(), first);
            assert_eq!(cancelled.view()?.completed_runs(), 0);

            let replacement = client
                .call::<Schedule>(ScheduleRequest::new(second, Trigger::once(future)))?
                .await?;
            assert!(replacement.is_ok());
            self.done.set(true);
            pending().await
        })
    }

    fn unregister(&mut self, _context: &mut UnregisterContext<'_>) -> ComponentResult<()> {
        Ok(())
    }
}

#[test]
fn scheduler_mutation_rpcs_enforce_time_identity_capacity_and_cancellation() {
    futures_lite::future::block_on(install_global_memory_vfs()).expect("install global test VFS");
    let lanes = Box::leak(Box::new(RpcLaneStorage::<8, FRAME_SIZE, 8>::new()));
    let mut router =
        futures_lite::future::block_on(EventRouter::new(lanes)).expect("create Event Router");
    let time = TimeComponent::new(TimeConfig::new(10, 60_000, 120_000));
    time.shared_state()
        .borrow_mut()
        .synchronize(SyncSample::new(1_800_000_000_000, Instant::now()));
    router.load(Box::new(time)).expect("load time Component");
    router
        .load(Box::new(SchedulerComponent::new(SchedulerConfig::new(
            1, 10,
        ))))
        .expect("load scheduler Component");
    let done = Rc::new(Cell::new(false));
    router
        .load(Box::new(MutationDriver {
            done: Rc::clone(&done),
        }))
        .expect("load mutation driver");

    futures_lite::future::block_on(poll_fn(|context| {
        if let Poll::Ready(result) = Pin::new(&mut router).poll(context) {
            return Poll::Ready(result);
        }
        if done.get() {
            Poll::Ready(Ok(()))
        } else {
            Poll::Pending
        }
    }))
    .expect("drive scheduler mutations");
}
