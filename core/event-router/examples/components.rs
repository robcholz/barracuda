//! Usage: host Components, call their typed RPCs, and manage their lifecycle.

use std::cell::Cell;
use std::future::{pending, poll_fn, Future};
use std::pin::Pin;
use std::rc::Rc;
use std::task::Poll;

use barracuda_event_router::{
    Component, ComponentFuture, ComponentResult, EventRouter, RegisterContext, RpcError, RpcFrame,
    RpcLaneStorage, RpcMethod, RunContext, Unary, UnregisterContext,
};
use barracuda_platform_test::install_global_memory_vfs;
use static_cell::ConstStaticCell;
use zerocopy::{Immutable, IntoBytes, KnownLayout, TryFromBytes};

const FRAME_SIZE: usize = 256;

static RPC_LANES: ConstStaticCell<RpcLaneStorage<2, FRAME_SIZE, 4>> =
    ConstStaticCell::new(RpcLaneStorage::new());

#[repr(C)]
#[derive(Clone, Copy, Debug, Immutable, IntoBytes, KnownLayout, PartialEq, Eq, TryFromBytes)]
struct Number {
    value: u32,
}

struct Increment;

impl RpcMethod for Increment {
    const ADDRESS: &'static str = "counter.increment";
    type Request = Number;
    type Response = Number;
    type Error = ();
    type Input = Unary;
    type Output = Unary;
}

#[derive(Default)]
struct ReviewState {
    response: Cell<Option<Number>>,
    service_unregistered: Cell<bool>,
    caller_unregistered: Cell<bool>,
}

struct CounterService {
    state: Rc<ReviewState>,
}

impl Component<FRAME_SIZE> for CounterService {
    fn register(&mut self, context: &mut RegisterContext<'_, FRAME_SIZE>) -> ComponentResult<()> {
        context.register_rpc::<Increment, _>(|_context, request: RpcFrame<Number>| async move {
            Ok(Ok(Number {
                value: request.view()?.value.saturating_add(1),
            }))
        })
    }

    fn run<'a>(&'a mut self, _context: RunContext<FRAME_SIZE>) -> ComponentFuture<'a> {
        Box::pin(pending())
    }

    fn unregister(&mut self, _context: &mut UnregisterContext<'_>) -> ComponentResult<()> {
        self.state.service_unregistered.set(true);
        Ok(())
    }
}

struct StartupCaller {
    state: Rc<ReviewState>,
}

impl Component<FRAME_SIZE> for StartupCaller {
    fn register(&mut self, _context: &mut RegisterContext<'_, FRAME_SIZE>) -> ComponentResult<()> {
        Ok(())
    }

    fn run<'a>(&'a mut self, context: RunContext<FRAME_SIZE>) -> ComponentFuture<'a> {
        Box::pin(async move {
            let response = context
                .rpc()
                .call::<Increment>(Number { value: 41 })?
                .await?
                .map_err(|_method_error| RpcError::InvalidFrameState)?;
            self.state.response.set(Some(*response.view()?));
            pending().await
        })
    }

    fn unregister(&mut self, _context: &mut UnregisterContext<'_>) -> ComponentResult<()> {
        self.state.caller_unregistered.set(true);
        Ok(())
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn core::error::Error>> {
    let state = Rc::new(ReviewState::default());
    install_global_memory_vfs().await?;
    let mut event_router = EventRouter::new(RPC_LANES.take()).await?;

    let service = event_router.load(Box::new(CounterService {
        state: Rc::clone(&state),
    }))?;
    let caller = event_router.load(Box::new(StartupCaller {
        state: Rc::clone(&state),
    }))?;

    poll_fn(|context| {
        if let Poll::Ready(result) = Pin::new(&mut event_router).poll(context) {
            return Poll::Ready(result);
        }
        if state.response.get().is_some() {
            Poll::Ready(Ok(()))
        } else {
            Poll::Pending
        }
    })
    .await?;

    assert_eq!(state.response.get(), Some(Number { value: 42 }));

    event_router.unload(caller)?;
    event_router.unload(service)?;
    assert!(state.caller_unregistered.get());
    assert!(state.service_unregistered.get());

    println!("Component RPC returned 42; both Components unregistered");
    Ok(())
}
