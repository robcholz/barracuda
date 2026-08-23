#![allow(missing_docs)]
#![allow(clippy::expect_used)]
#![allow(clippy::panic)]

use std::cell::{Cell, RefCell};
use std::future::{Future, pending};
use std::pin::Pin;
use std::rc::Rc;
use std::task::Poll;

use barracuda_event_router::{
    Component, ComponentError, ComponentFuture, ComponentResult, EventRouter, MemFs,
    RegisterContext, RpcLaneStorage, RpcStream, RunContext, UnregisterContext,
};
use barracuda_vm_component::component::VmComponent;
use barracuda_vm_component::run::{ChunkBoundary, Run, RunRequestFrame};
use futures_lite::stream;

#[derive(Default)]
struct ResultState {
    done: Cell<bool>,
    output: RefCell<String>,
}

struct VmClient {
    result: Rc<ResultState>,
}

impl Component<64> for VmClient {
    fn register(&mut self, _context: &mut RegisterContext<'_, 64>) -> ComponentResult<()> {
        Ok(())
    }

    fn run<'a>(&'a mut self, context: RunContext<64>) -> ComponentFuture<'a> {
        Box::pin(async move {
            let requests = [
                RunRequestFrame::source(
                    "local io=require('io'); local name=io.in",
                    ChunkBoundary::More,
                )
                .map_err(ComponentError::lifecycle)?,
                RunRequestFrame::source("put(); io.print('hello', name)", ChunkBoundary::Complete)
                    .map_err(ComponentError::lifecycle)?,
                RunRequestFrame::input("barra", ChunkBoundary::More)
                    .map_err(ComponentError::lifecycle)?,
                RunRequestFrame::input("cuda", ChunkBoundary::Complete)
                    .map_err(ComponentError::lifecycle)?,
            ];
            let input = RpcStream::new(stream::iter(requests.into_iter().map(Ok)));
            let mut output = context.rpc().call::<Run>(input)?;
            while let Some(item) = output.next().await {
                let frame = match item? {
                    Ok(frame) => *frame.view()?,
                    Err(error) => panic!("vm.run method error: {:?}", error.view()?),
                };
                self.result
                    .output
                    .borrow_mut()
                    .push_str(frame.text().map_err(ComponentError::lifecycle)?);
            }
            self.result.done.set(true);
            pending().await
        })
    }

    fn unregister(&mut self, _context: &mut UnregisterContext<'_>) -> ComponentResult<()> {
        Ok(())
    }
}

#[test]
fn vm_component_runs_chunked_source_and_input_through_event_router() {
    futures_lite::future::block_on(async {
        let lanes = Box::leak(Box::new(RpcLaneStorage::<4, 64, 4>::new()));
        let filesystem = Box::leak(Box::new(MemFs::new()));
        let mut router =
            EventRouter::new(lanes, filesystem, "workflows").expect("build Event Router");
        router
            .load(Box::new(VmComponent::default()))
            .expect("load VM Component");

        let result = Rc::new(ResultState::default());
        router
            .load(Box::new(VmClient {
                result: Rc::clone(&result),
            }))
            .expect("load VM client");

        core::future::poll_fn(|context| {
            if let Poll::Ready(Err(error)) = Pin::new(&mut router).poll(context) {
                panic!("Event Router failed: {error}");
            }
            if result.done.get() {
                Poll::Ready(())
            } else {
                Poll::Pending
            }
        })
        .await;

        assert_eq!(result.output.borrow().as_str(), "hello\tbarracuda");
    });
}
