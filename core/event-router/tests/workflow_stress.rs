#![allow(clippy::expect_used)]
#![allow(missing_docs)]

mod support;

use core::cell::{Cell, RefCell};
use std::future::pending;
use std::rc::Rc;
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant};

use barracuda_event_router::{
    Component, ComponentFuture, ComponentResult, EmitError, Event, EventEmitter, EventRouter,
    JsonRpcSchema, JsonSchema, JsonWriter, RegisterContext, RpcError, RpcLaneStorage, RunContext,
    UnregisterContext, WorkflowClient,
};
use barracuda_platform_test::install_global_memory_vfs;
use barracuda_vfs::remove_file;
use futures_lite::future::{block_on, yield_now};

const FRAME_CAPACITY: usize = 256;
const EMPTY_SCHEMA: JsonSchema =
    JsonSchema::new(r#"{"type":"object","properties":{},"additionalProperties":false}"#);
static GLOBAL_VFS_TEST_LOCK: Mutex<()> = Mutex::new(());
const WORKFLOW_JSON: &str = r#"{
    "id": "stress-workflow",
    "match": { "event": "stress.event" },
    "steps": [{ "call": "stress.sink" }]
}"#;

struct StressEvent;

impl Event for StressEvent {
    const ID: &'static str = "stress.event";
}

struct Sink;

impl JsonRpcSchema for Sink {
    const ADDRESS: &'static str = "stress.sink";
    const REQUEST_SCHEMA: JsonSchema = EMPTY_SCHEMA;
    const RESPONSE_SCHEMA: JsonSchema = EMPTY_SCHEMA;
    const MAX_REQUEST_BYTES: usize = 2;
    const MAX_RESPONSE_BYTES: usize = 2;
}

#[derive(Default)]
struct State {
    done: Cell<bool>,
    received: Cell<usize>,
    elapsed: Cell<Option<Duration>>,
    error: RefCell<Option<EmitError>>,
}

struct Producer {
    events: usize,
    cooperative: bool,
    state: Rc<State>,
}

impl Component<FRAME_CAPACITY> for Producer {
    fn name(&self) -> &'static str {
        "producer"
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
            WorkflowClient::<FRAME_CAPACITY>::new(context.rpc().clone())
                .load(WORKFLOW_JSON)
                .await
                .map_err(barracuda_event_router::ComponentError::lifecycle)?;
            let emitter = EventEmitter::<FRAME_CAPACITY>::new(context.rpc().clone());
            let started = Instant::now();
            for _event in 0..self.events {
                if let Err(error) = emitter.emit::<StressEvent>("{}").await {
                    self.state.error.replace(Some(error));
                    break;
                }
                if self.cooperative {
                    yield_now().await;
                }
            }
            self.state.elapsed.set(Some(started.elapsed()));
            self.state.done.set(true);
            pending().await
        })
    }

    fn unregister(&mut self, _context: &mut UnregisterContext<'_>) -> ComponentResult<()> {
        Ok(())
    }
}

fn reset_global_vfs() -> MutexGuard<'static, ()> {
    let guard = GLOBAL_VFS_TEST_LOCK.lock().expect("lock global VFS test");
    block_on(install_global_memory_vfs()).expect("install global test VFS");
    let _ignored = block_on(remove_file("/system/workflows.json"));
    guard
}

fn new_router<const N: usize, const Q: usize>() -> EventRouter<N, FRAME_CAPACITY, Q> {
    let lanes = Box::leak(Box::new(RpcLaneStorage::<N, FRAME_CAPACITY, Q>::new()));
    block_on(EventRouter::new(lanes)).expect("create Event Router")
}

#[test]
fn cooperative_producer_drives_thousands_of_events_through_two_lanes() {
    let _global_vfs = reset_global_vfs();
    const EVENTS: usize = 2_000;

    let state = Rc::new(State::default());
    let mut event_router = new_router::<2, 2>();
    event_router
        .load(Box::new(Producer {
            events: EVENTS,
            cooperative: true,
            state: Rc::clone(&state),
        }))
        .expect("load cooperative producer");

    let polls = support::drive_until(&mut event_router, |router| {
        state.done.get()
            && (state.error.borrow().is_some() || router.workflow_info().completed_count == EVENTS)
    })
    .expect("drive cooperative Event Router");

    assert!(state.error.borrow().is_none());
    assert_eq!(state.received.get(), EVENTS);
    assert_eq!(event_router.workflow_info().completed_count, EVENTS);
    let elapsed = state.elapsed.get().expect("producer elapsed time");
    eprintln!(
        "event_router_cooperative: events={EVENTS}, polls={polls}, elapsed_ns={}, accepted_events_per_second={:.0}",
        elapsed.as_nanos(),
        EVENTS as f64 / elapsed.as_secs_f64()
    );
}

#[test]
fn non_cooperative_burst_is_bounded_by_fixed_event_input_storage() {
    let _global_vfs = reset_global_vfs();
    const LANES: usize = 4;

    let state = Rc::new(State::default());
    let mut event_router = new_router::<LANES, 4>();
    event_router
        .load(Box::new(Producer {
            events: 16,
            cooperative: false,
            state: Rc::clone(&state),
        }))
        .expect("load burst producer");

    support::drive_until(&mut event_router, |router| {
        state.done.get() && router.workflow_info().completed_count == LANES
    })
    .expect("drive burst Event Router");

    assert!(matches!(
        state.error.borrow().as_ref(),
        Some(EmitError::Rpc(RpcError::ResourceExhausted {
            resource: "Workflow Event inputs",
            limit: LANES,
        }))
    ));
    assert_eq!(state.received.get(), LANES);
    assert_eq!(event_router.workflow_info().completed_count, LANES);
}
