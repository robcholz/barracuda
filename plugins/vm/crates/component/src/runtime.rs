use alloc::rc::Rc;
use core::cell::Cell;

use async_channel::{Receiver, Sender};
use barracuda_event_router::{RpcFrame, RpcHandler, RpcResult, RpcStream};
use embassy_executor::Spawner;
use embassy_time::Timer;
use futures_core::Stream;

use crate::VmLimits;
use crate::memory::{VmMemoryLease, VmMemoryPool, VmMemoryPoolError};
use crate::run::{Run, RunError, RunErrorKind, RunItem, RunRequestFrame, run_stream};
use barracuda_vm_builtin_packages::BuiltinPackages;

/// Number of statically allocated Embassy task slots available to Lua executions.
pub const VM_TASK_SLOTS: usize = 4;
/// Delay applied by the VM Embassy task after every instruction-hook yield.
pub const VM_YIELD_DELAY_MILLIS: u64 = 100;
/// Default fixed Lua heap size owned by each VM memory-pool slot.
pub const VM_MEMORY_BYTES_PER_SLOT: usize = 64 * 1024;

const RESPONSE_QUEUE_CAPACITY: usize = 1;

#[derive(Clone, Default)]
pub(crate) struct VmYieldSignal(Rc<Cell<bool>>);

impl VmYieldSignal {
    fn new() -> Self {
        Self::default()
    }

    pub(crate) fn mark(&self) {
        self.0.set(true);
    }

    fn take(&self) -> bool {
        self.0.replace(false)
    }
}

/// Shared startup handle used by the VM Component to dispatch executions.
#[derive(Clone)]
pub struct VmRuntime {
    spawner: Rc<Cell<Option<Spawner>>>,
    memory_pool: VmMemoryPool,
}

impl VmRuntime {
    /// Creates an unstarted VM runtime with the default per-VM Lua heap size.
    ///
    /// # Errors
    ///
    /// Returns an error when backing storage for the memory pool cannot be reserved.
    pub fn new() -> Result<Self, VmMemoryPoolError> {
        Self::with_memory_bytes(VM_MEMORY_BYTES_PER_SLOT)
    }

    /// Creates an unstarted VM runtime with an explicit fixed Lua heap size per slot.
    ///
    /// # Errors
    ///
    /// Returns an error when the size is invalid or backing storage cannot be reserved.
    pub fn with_memory_bytes(bytes: usize) -> Result<Self, VmMemoryPoolError> {
        Ok(Self {
            spawner: Rc::new(Cell::new(None)),
            memory_pool: VmMemoryPool::new(VM_TASK_SLOTS, bytes)?,
        })
    }

    /// Installs the System-provided Embassy spawner after Plugin registration completes.
    ///
    /// # Errors
    ///
    /// Returns [`VmRuntimeStartError::AlreadyStarted`] when called more than once.
    pub fn start(&self, spawner: Spawner) -> Result<(), VmRuntimeStartError> {
        if self.spawner.get().is_some() {
            return Err(VmRuntimeStartError::AlreadyStarted);
        }
        self.spawner.set(Some(spawner));
        Ok(())
    }

    fn dispatch(
        &self,
        requests: RpcStream<RpcFrame<RunRequestFrame>>,
        limits: VmLimits,
        builtin_packages: BuiltinPackages,
    ) -> RpcStream<RunItem> {
        let Some(spawner) = self.spawner.get() else {
            return error_stream(
                RunErrorKind::RuntimeUnavailable,
                "VM Embassy runtime has not started",
            );
        };
        let Some(memory) = self.memory_pool.acquire() else {
            return error_stream(RunErrorKind::Busy, "all VM memory slots are occupied");
        };
        let (sender, receiver) = async_channel::bounded(RESPONSE_QUEUE_CAPACITY);
        let fallback = sender.clone();
        if spawner
            .spawn(vm_execution_task(
                requests,
                limits,
                builtin_packages,
                sender,
                memory,
            ))
            .is_err()
        {
            let _result = fallback.try_send(Ok(Err(RunError::new(
                RunErrorKind::Busy,
                "all VM task slots are occupied",
            ))));
        }
        drop(fallback);
        RpcStream::new(receiver)
    }
}

/// Failure while starting one VM runtime.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum VmRuntimeStartError {
    /// The runtime already owns an Embassy spawner.
    #[error("VM runtime has already started")]
    AlreadyStarted,
}

pub(crate) fn task_run_handler(
    runtime: VmRuntime,
    limits: VmLimits,
    builtin_packages: BuiltinPackages,
) -> impl RpcHandler<Run> {
    move |_context, requests: RpcStream<RpcFrame<RunRequestFrame>>| {
        let runtime = runtime.clone();
        async move { Ok(runtime.dispatch(requests, limits, builtin_packages)) }
    }
}

#[embassy_executor::task(pool_size = VM_TASK_SLOTS)]
async fn vm_execution_task(
    requests: RpcStream<RpcFrame<RunRequestFrame>>,
    limits: VmLimits,
    builtin_packages: BuiltinPackages,
    responses: Sender<RpcResult<RunItem>>,
    memory: VmMemoryLease,
) {
    let yield_signal = VmYieldSignal::new();
    let mut stream = core::pin::pin!(run_stream(
        requests,
        limits,
        builtin_packages,
        Some(yield_signal.clone()),
        Some(memory),
    ));
    loop {
        let event = core::future::poll_fn(|context| {
            if responses.is_closed() {
                return core::task::Poll::Ready(VmTaskEvent::Closed);
            }
            match stream.as_mut().poll_next(context) {
                core::task::Poll::Ready(Some(item)) => {
                    core::task::Poll::Ready(VmTaskEvent::Item(item))
                }
                core::task::Poll::Ready(None) => core::task::Poll::Ready(VmTaskEvent::Complete),
                core::task::Poll::Pending if yield_signal.take() => {
                    core::task::Poll::Ready(VmTaskEvent::Yielded)
                }
                core::task::Poll::Pending => core::task::Poll::Pending,
            }
        })
        .await;
        match event {
            VmTaskEvent::Item(item) => {
                if responses.send(item).await.is_err() {
                    return;
                }
            }
            VmTaskEvent::Yielded => Timer::after_millis(VM_YIELD_DELAY_MILLIS).await,
            VmTaskEvent::Complete | VmTaskEvent::Closed => return,
        }
    }
}

enum VmTaskEvent {
    Item(RpcResult<RunItem>),
    Yielded,
    Complete,
    Closed,
}

fn error_stream(kind: RunErrorKind, diagnostic: &str) -> RpcStream<RunItem> {
    let (sender, receiver): (Sender<RpcResult<RunItem>>, Receiver<RpcResult<RunItem>>) =
        async_channel::bounded(RESPONSE_QUEUE_CAPACITY);
    let _result = sender.try_send(Ok(Err(RunError::new(kind, diagnostic))));
    drop(sender);
    RpcStream::new(receiver)
}
