#![allow(clippy::expect_used)]
#![allow(missing_docs)]

mod support;

use core::cell::{Cell, RefCell};
use std::future::pending;
use std::rc::Rc;

use barracuda_event_router::{
    Component, ComponentFuture, ComponentResult, EmitError, Event, EventEmitter, EventRouter,
    MemFs, RegisterContext, RpcError, RpcFrame, RpcLaneStorage, RpcMethod, RunContext, Unary,
    UnregisterContext, WorkflowClient, WorkflowControlError,
};

const FRAME_CAPACITY: usize = 64;
const DIRECTORY: &str = "capacity-workflows";

struct CapacityEvent;

impl Event for CapacityEvent {
    const ID: &'static str = "capacity.event";
    type Message = [u8; 1];
    type Input = Unary;
}

struct Sink;

impl RpcMethod for Sink {
    const ADDRESS: &'static str = "capacity.sink";
    type Request = [u8; 1];
    type Response = ();
    type Error = ();
    type Input = Unary;
    type Output = Unary;
}

#[derive(Default)]
struct State {
    done: Cell<bool>,
    received: Cell<usize>,
    failure: RefCell<Option<Result<EmitError, WorkflowControlError>>>,
}

struct FanoutComponent {
    workflows: usize,
    state: Rc<State>,
}

impl Component<FRAME_CAPACITY> for FanoutComponent {
    fn register(
        &mut self,
        context: &mut RegisterContext<'_, FRAME_CAPACITY>,
    ) -> ComponentResult<()> {
        let state = Rc::clone(&self.state);
        context.register_rpc::<Sink, _>(move |_context, request: RpcFrame<[u8; 1]>| {
            let state = Rc::clone(&state);
            async move {
                request.view()?;
                state.received.set(state.received.get().saturating_add(1));
                Ok(Ok(()))
            }
        })
    }

    fn run<'a>(&'a mut self, context: RunContext<FRAME_CAPACITY>) -> ComponentFuture<'a> {
        Box::pin(async move {
            let workflow = WorkflowClient::<FRAME_CAPACITY>::new(context.rpc().clone());
            for index in 0..self.workflows {
                let json = format!(
                    r#"{{"id":"capacity-{index}","match":{{"event":"capacity.event"}},"steps":[{{"call":"capacity.sink"}}]}}"#
                );
                if let Err(error) = workflow.load(&json).await {
                    self.state.failure.replace(Some(Err(error)));
                    self.state.done.set(true);
                    pending::<()>().await;
                }
            }

            if let Err(error) = EventEmitter::<FRAME_CAPACITY>::new(context.rpc().clone())
                .emit::<CapacityEvent>([7])
                .await
            {
                self.state.failure.replace(Some(Ok(error)));
            }
            self.state.done.set(true);
            pending().await
        })
    }

    fn unregister(&mut self, _context: &mut UnregisterContext<'_>) -> ComponentResult<()> {
        Ok(())
    }
}

fn new_router<const N: usize, const Q: usize>(
    filesystem: &'static MemFs,
) -> EventRouter<N, FRAME_CAPACITY, Q> {
    let lanes = Box::leak(Box::new(RpcLaneStorage::<N, FRAME_CAPACITY, Q>::new()));
    EventRouter::new(lanes, filesystem, DIRECTORY).expect("create Event Router")
}

#[test]
fn matched_fanout_can_use_every_lane_except_the_ingress_lane() {
    let filesystem = Box::leak(Box::new(MemFs::new()));
    let state = Rc::new(State::default());
    let mut event_router = new_router::<4, 4>(filesystem);
    event_router
        .load(Box::new(FanoutComponent {
            workflows: 3,
            state: Rc::clone(&state),
        }))
        .expect("load fanout Component");

    support::drive_until(&mut event_router, |router| {
        state.done.get() && router.workflow_info().completed_count == 3
    })
    .expect("drive Event Router to expected state");

    assert!(state.failure.borrow().is_none());
    assert_eq!(state.received.get(), 3);
    assert_eq!(event_router.workflow_info().completed_count, 3);
}

#[test]
fn matched_fanout_equal_to_lane_count_is_rejected_as_nested_exhaustion() {
    let filesystem = Box::leak(Box::new(MemFs::new()));
    let state = Rc::new(State::default());
    let mut event_router = new_router::<4, 4>(filesystem);
    event_router
        .load(Box::new(FanoutComponent {
            workflows: 4,
            state: Rc::clone(&state),
        }))
        .expect("load fanout Component");

    support::drive_until(&mut event_router, |_router| state.done.get())
        .expect("drive Event Router to expected state");

    assert!(matches!(
        state.failure.borrow().as_ref(),
        Some(Ok(EmitError::Rpc(RpcError::NestedLaneExhausted {
            limit: 4
        })))
    ));
    assert_eq!(state.received.get(), 0);
    assert_eq!(event_router.workflow_info().completed_count, 0);
}
