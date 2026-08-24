//! Usage: a multi-step Workflow maps a field from one RPC response into the
//! next RPC request with `$previous.output.<field>`.

use std::cell::{Cell, RefCell};
use std::future::{pending, poll_fn, Future};
use std::pin::Pin;
use std::rc::Rc;
use std::task::Poll;

use barracuda_event_router::{
    rpc_dynamic, Component, ComponentError, ComponentFuture, ComponentResult, Event, EventEmitter,
    EventRouter, RegisterContext, RpcFrame, RpcLaneStorage, RpcMethod, RpcWire, RunContext, Unary,
    UnregisterContext, WorkflowClient,
};
use barracuda_platform_test::MemFs;
use serde::{Deserialize, Serialize};
use static_cell::ConstStaticCell;
use zerocopy::{Immutable, IntoBytes, KnownLayout, TryFromBytes};

const FRAME_SIZE: usize = 256;

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

#[repr(C)]
#[derive(
    Serialize,
    Deserialize,
    Clone,
    Copy,
    Debug,
    Immutable,
    IntoBytes,
    KnownLayout,
    PartialEq,
    Eq,
    RpcWire,
    TryFromBytes,
)]
struct Seed {
    n: u32,
}

#[repr(C)]
#[derive(
    Serialize,
    Deserialize,
    Clone,
    Copy,
    Debug,
    Immutable,
    IntoBytes,
    KnownLayout,
    PartialEq,
    Eq,
    RpcWire,
    TryFromBytes,
)]
struct Reply {
    token: u32,
}

#[repr(C)]
#[derive(
    Serialize,
    Deserialize,
    Clone,
    Copy,
    Debug,
    Immutable,
    IntoBytes,
    KnownLayout,
    PartialEq,
    Eq,
    RpcWire,
    TryFromBytes,
)]
struct Deliver {
    // Filled from `$previous.output.token`, so it is absent from the literal
    // arguments and must default when the base request is transcoded.
    #[serde(default)]
    token: u32,
    extra: u32,
}

struct SeedEvent;

impl Event for SeedEvent {
    const ID: &'static str = "example.seed";
    type Message = Seed;
    type Input = Unary;
}

struct Produce;

#[rpc_dynamic]
impl RpcMethod for Produce {
    const ADDRESS: &'static str = "example.produce";
    type Request = Seed;
    type Response = Reply;
    type Error = ();
    type Input = Unary;
    type Output = Unary;
}

struct Consume;

#[rpc_dynamic]
impl RpcMethod for Consume {
    const ADDRESS: &'static str = "example.consume";
    type Request = Deliver;
    type Response = ();
    type Error = ();
    type Input = Unary;
    type Output = Unary;
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
        context.register_rpc::<Produce, _>(|_context, request: RpcFrame<Seed>| async move {
            let seed = *request.view()?;
            Ok(Ok(Reply { token: seed.n }))
        })?;
        let recorded = Rc::clone(&self.state.recorded);
        context.register_rpc::<Consume, _>(move |_context, request: RpcFrame<Deliver>| {
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
                .load(WORKFLOW_JSON)
                .await
                .map_err(ComponentError::lifecycle)?;
            EventEmitter::<FRAME_SIZE>::new(client)
                .emit::<SeedEvent>(Seed { n: 41 })
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
    let filesystem = MemFs::new();
    let mut event_router = EventRouter::new(RPC_LANES.take(), filesystem, "workflows")?;

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
    let info = event_router.workflow_info();
    assert_eq!(info.completed_count, 1);
    assert_eq!(info.failed_count, 0);
    assert_eq!(event_router.workflow_definitions().len(), 1);

    event_router.unload(demo)?;
    println!("Workflow mapped `$previous.output.token` into the next request");
    Ok(())
}
