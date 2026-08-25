#![allow(clippy::expect_used)]
#![allow(clippy::panic)]
#![allow(missing_docs)]

use std::future::{Future, pending};
use std::pin::Pin;
use std::sync::mpsc::{SyncSender, sync_channel};
use std::task::Poll;
use std::time::{Duration, Instant};

use barracuda_event_router::{
    Component, ComponentError, ComponentFuture, ComponentResult, EventRouter, RegisterContext,
    RpcLaneStorage, RpcStream, RunContext, UnregisterContext,
};
use barracuda_platform_test::install_global_memory_vfs;
use barracuda_vm_component::run::{ChunkBoundary, Run, RunErrorKind, RunRequestFrame};
use barracuda_vm_component::{
    BuiltinPackages, VM_TASK_SLOTS, VM_YIELD_DELAY_MILLIS, VmComponent, VmLimits, VmRuntime,
};
use embassy_executor::{Executor, Spawner};
use futures_lite::{StreamExt as _, future::poll_once, stream};

struct PoolClient {
    completed: SyncSender<Result<(), String>>,
}

impl Component<64> for PoolClient {
    fn register(&mut self, _context: &mut RegisterContext<'_, 64>) -> ComponentResult<()> {
        Ok(())
    }

    fn run<'a>(&'a mut self, context: RunContext<64>) -> ComponentFuture<'a> {
        Box::pin(async move {
            let source = RunRequestFrame::source(
                "local s='x'; for i=1,20 do s=s..s end",
                ChunkBoundary::Complete,
            )
            .map_err(ComponentError::lifecycle)?;
            let requests = RpcStream::new(stream::once(Ok(source)));
            let mut exhausted = context.rpc().call::<Run>(requests)?;
            let error = exhausted
                .next()
                .await
                .expect("Lua memory error response")?
                .expect_err("script must exceed its fixed Lua heap");
            assert_eq!(error.view()?.kind(), RunErrorKind::LuaMemory);
            futures_lite::future::yield_now().await;

            let source = RunRequestFrame::source(
                "local n=0; for i=1,100 do n=n+i end",
                ChunkBoundary::Complete,
            )
            .map_err(ComponentError::lifecycle)?;
            let requests = RpcStream::new(stream::once(Ok(source)));
            let started = Instant::now();
            let mut delayed = context.rpc().call::<Run>(requests)?;
            while delayed.next().await.is_some() {}
            assert!(started.elapsed() >= Duration::from_millis(VM_YIELD_DELAY_MILLIS));
            futures_lite::future::yield_now().await;

            let mut occupied = Vec::new();
            for _slot in 0..VM_TASK_SLOTS {
                let source = RunRequestFrame::source("while true do end", ChunkBoundary::Complete)
                    .map_err(ComponentError::lifecycle)?;
                let requests = RpcStream::new(stream::once(Ok(source)).chain(stream::pending()));
                let mut output = context.rpc().call::<Run>(requests)?;
                let observed = poll_once(output.next()).await;
                assert!(observed.is_none(), "unexpected VM response: {observed:?}");
                occupied.push(output);
            }

            let source = RunRequestFrame::source("return", ChunkBoundary::Complete)
                .map_err(ComponentError::lifecycle)?;
            let requests = RpcStream::new(stream::once(Ok(source)));
            let mut overflow = context.rpc().call::<Run>(requests)?;
            let error = overflow
                .next()
                .await
                .expect("busy response")?
                .expect_err("the fifth concurrent VM must be rejected");
            assert_eq!(error.view()?.kind(), RunErrorKind::Busy);

            drop(occupied.pop());
            embassy_time::Timer::after_millis(VM_YIELD_DELAY_MILLIS + 1).await;
            let source = RunRequestFrame::source("return", ChunkBoundary::Complete)
                .map_err(ComponentError::lifecycle)?;
            let requests = RpcStream::new(stream::once(Ok(source)));
            let mut replacement = context.rpc().call::<Run>(requests)?;
            assert!(
                replacement.next().await.is_none(),
                "dropping an execution handle must release its VM task slot"
            );

            let _result = self.completed.send(Ok(()));
            let _occupied = occupied;
            pending().await
        })
    }

    fn unregister(&mut self, _context: &mut UnregisterContext<'_>) -> ComponentResult<()> {
        Ok(())
    }
}

#[embassy_executor::task]
async fn exercise_pool(spawner: Spawner, completed: SyncSender<Result<(), String>>) {
    let runtime = VmRuntime::new().expect("create VM memory pool");
    runtime.start(spawner).expect("start VM runtime");
    let lanes = Box::leak(Box::new(RpcLaneStorage::<8, 64, 8>::new()));
    install_global_memory_vfs()
        .await
        .expect("install global test VFS");
    let mut router = EventRouter::new(lanes).await.expect("create Event Router");
    router
        .load(Box::new(
            VmComponent::with_runtime(BuiltinPackages::all(), runtime)
                .with_limits(VmLimits::default().with_instruction_hook_interval(100)),
        ))
        .expect("load VM Component");
    router
        .load(Box::new(PoolClient { completed }))
        .expect("load VM pool client");

    core::future::poll_fn(|context| match Pin::new(&mut router).poll(context) {
        Poll::Ready(result) => panic!("Event Router stopped: {result:?}"),
        Poll::Pending => Poll::<()>::Pending,
    })
    .await;
}

#[test]
fn embassy_task_waits_after_hook_yields_and_supports_four_concurrent_vms() {
    let (completed, result) = sync_channel(1);
    std::thread::spawn(move || {
        let executor = Box::leak(Box::new(Executor::new()));
        executor.run(|spawner| {
            spawner
                .spawn(exercise_pool(spawner, completed))
                .expect("spawn VM pool test");
        });
    });

    result
        .recv_timeout(Duration::from_secs(5))
        .expect("VM pool test timed out")
        .expect("VM pool test failed");
}
