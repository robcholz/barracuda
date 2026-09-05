#![allow(clippy::expect_used)]
#![allow(missing_docs)]

mod support;

use core::cell::{Cell, RefCell};
use std::future::pending;
use std::rc::Rc;
use std::sync::{Mutex, MutexGuard};

use barracuda_event_router::{
    Component, ComponentFuture, ComponentResult, EmitError, Event, EventEmitter, EventRouter,
    JsonRpcSchema, JsonSchema, JsonWriter, RegisterContext, RpcLaneStorage, RunContext,
    UnregisterContext, WorkflowClient, WorkflowControlError,
};
use barracuda_platform_test::install_global_memory_vfs;
use barracuda_vfs::remove_file;
use futures_lite::future::block_on;

const FRAME_CAPACITY: usize = 256;
const EMPTY_SCHEMA: JsonSchema = barracuda_rpc::json_schema_inline!(
    r#"{"type":"object","properties":{},"additionalProperties":false}"#
);
static GLOBAL_VFS_TEST_LOCK: Mutex<()> = Mutex::new(());

fn reset_global_vfs() -> MutexGuard<'static, ()> {
    let guard = GLOBAL_VFS_TEST_LOCK.lock().expect("lock global VFS test");
    block_on(install_global_memory_vfs()).expect("install global test VFS");
    let _ignored = block_on(remove_file("/system/workflows.json"));
    guard
}

struct CapacityEvent;

impl Event for CapacityEvent {
    const ID: &'static str = "capacity.event";
}

struct Sink;

impl JsonRpcSchema for Sink {
    const ADDRESS: &'static str = "capacity.sink";
    const REQUEST_SCHEMA: JsonSchema = EMPTY_SCHEMA;
    const RESPONSE_SCHEMA: JsonSchema = EMPTY_SCHEMA;
    const MAX_REQUEST_BYTES: usize = 2;
    const MAX_RESPONSE_BYTES: usize = 2;
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
    fn name(&self) -> &'static str {
        "fanout"
    }

    fn register(
        &mut self,
        context: &mut RegisterContext<'_, FRAME_CAPACITY>,
    ) -> ComponentResult<()> {
        let state = Rc::clone(&self.state);
        context.register_json::<Sink, _>("*", move |_context, _request, response: JsonWriter| {
            let state = Rc::clone(&state);
            async move {
                state.received.set(state.received.get().saturating_add(1));
                response.write("{}").await
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
                .emit::<CapacityEvent>("{}")
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

fn new_router<const N: usize, const Q: usize>() -> EventRouter<N, FRAME_CAPACITY, Q> {
    let lanes = Box::leak(Box::new(RpcLaneStorage::<N, FRAME_CAPACITY, Q>::new()));
    block_on(EventRouter::new(lanes)).expect("create Event Router")
}

#[test]
fn fanout_can_use_every_lane_after_event_ingress_returns() {
    let _global_vfs = reset_global_vfs();
    let state = Rc::new(State::default());
    let mut event_router = new_router::<4, 4>();
    event_router
        .load(Box::new(FanoutComponent {
            workflows: 4,
            state: Rc::clone(&state),
        }))
        .expect("load fanout Component");

    support::drive_until(&mut event_router, |router| {
        state.done.get() && router.workflow_info().completed_count == 4
    })
    .expect("drive Event Router to expected state");

    assert!(state.failure.borrow().is_none());
    assert_eq!(state.received.get(), 4);
    assert_eq!(event_router.workflow_info().failed_count, 0);
}
