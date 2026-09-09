use alloc::rc::Rc;
use alloc::vec::Vec;
use core::cell::{Cell, RefCell};
use core::task::{Poll, Waker};

use barracuda_vm_builtin_packages::BuiltinPackages;
use barracuda_vm_package_api::LuaPackageRegistry;
use embassy_executor::Spawner;
use futures_channel::oneshot;

use crate::memory::{VmMemoryPool, VmMemoryPoolError};
use crate::run::{ExecutionJob, execute_run};
use crate::vm::vm_progress_channel;
use crate::{VmLimits, VmRun, VmRunInfo, VmRunState};

/// Number of statically allocated Embassy task slots available to Lua executions.
pub(crate) const VM_TASK_SLOTS: usize = 4;
/// Delay applied by the VM Embassy task after every instruction-hook yield.
pub(crate) const VM_YIELD_DELAY_MILLIS: u64 = 100;
/// Default fixed Lua heap size owned by each VM memory-pool slot.
pub(crate) const VM_MEMORY_BYTES_PER_SLOT: usize = 96 * 1024;

#[derive(Clone, Default)]
pub(crate) struct VmYieldSignal(Rc<Cell<bool>>);

impl VmYieldSignal {
    pub(crate) fn mark(&self) {
        self.0.set(true);
    }

    pub(crate) fn take(&self) -> bool {
        self.0.replace(false)
    }
}

pub(crate) struct InputMessage {
    value: alloc::string::String,
}

impl InputMessage {
    fn new(input: alloc::string::String) -> Self {
        Self { value: input }
    }

    pub(crate) fn as_str(&self) -> &str {
        &self.value
    }
}

struct RunSlot {
    active: Cell<bool>,
    id: Cell<u32>,
    input_open: Cell<bool>,
    waiting_for_input: Cell<bool>,
    cancelled: Cell<bool>,
    input: RefCell<Option<InputMessage>>,
    waiter: RefCell<Option<Waker>>,
}

impl RunSlot {
    fn new() -> Self {
        Self {
            active: Cell::new(false),
            id: Cell::new(0),
            input_open: Cell::new(false),
            waiting_for_input: Cell::new(false),
            cancelled: Cell::new(false),
            input: RefCell::new(None),
            waiter: RefCell::new(None),
        }
    }

    fn claim(&self, id: u32) {
        self.id.set(id);
        self.input_open.set(true);
        self.waiting_for_input.set(false);
        self.cancelled.set(false);
        *self.input.borrow_mut() = None;
        *self.waiter.borrow_mut() = None;
        self.active.set(true);
    }

    fn wake(&self) {
        if let Some(waker) = self.waiter.borrow_mut().take() {
            waker.wake();
        }
    }

    fn cancel(&self) {
        self.cancelled.set(true);
        self.input_open.set(false);
        self.waiting_for_input.set(false);
        self.wake();
    }

    fn release(&self, id: u32) {
        if self.active.get() && self.id.get() == id {
            self.cancelled.set(true);
            self.input_open.set(false);
            self.waiting_for_input.set(false);
            *self.input.borrow_mut() = None;
            *self.waiter.borrow_mut() = None;
            self.id.set(0);
            self.active.set(false);
        }
    }
}

struct RuntimeState {
    spawner: Cell<Option<Spawner>>,
    slots: [RunSlot; VM_TASK_SLOTS],
    next_run_id: Cell<u32>,
}

/// Shared startup and control handle for the fixed VM execution pool.
#[derive(Clone)]
pub struct VmRuntime {
    state: Rc<RuntimeState>,
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
            state: Rc::new(RuntimeState {
                spawner: Cell::new(None),
                slots: core::array::from_fn(|_index| RunSlot::new()),
                next_run_id: Cell::new(1),
            }),
            memory_pool: VmMemoryPool::new(VM_TASK_SLOTS, bytes)?,
        })
    }

    /// Installs the System-provided Embassy spawner after Plugin registration completes.
    ///
    /// # Errors
    ///
    /// Returns [`VmRuntimeStartError::AlreadyStarted`] when called more than once.
    pub fn start(&self, spawner: Spawner) -> Result<(), VmRuntimeStartError> {
        if self.state.spawner.get().is_some() {
            return Err(VmRuntimeStartError::AlreadyStarted);
        }
        self.state.spawner.set(Some(spawner));
        Ok(())
    }

    pub(crate) fn stop(&self) {
        self.state.spawner.set(None);
        for slot in &self.state.slots {
            if slot.active.get() {
                slot.cancel();
            }
        }
    }

    pub(crate) fn dispatch(
        &self,
        source: alloc::string::String,
        limits: VmLimits,
        builtin_packages: BuiltinPackages,
        package_registry: LuaPackageRegistry,
    ) -> Result<VmRun, DispatchError> {
        let spawner = self
            .state
            .spawner
            .get()
            .ok_or(DispatchError::RuntimeUnavailable)?;
        let memory = self.memory_pool.acquire().ok_or(DispatchError::Busy)?;
        let control = self.reserve_run()?;
        let run_id = control.run_id;
        let cancellation = control.cancellation();
        let (completion, result) = oneshot::channel();
        let (progress, updates) = vm_progress_channel();
        if spawner
            .spawn(vm_execution_task(ExecutionJob {
                run_id,
                source,
                control,
                memory,
                limits,
                builtin_packages,
                package_registry,
                progress,
                completion,
            }))
            .is_err()
        {
            return Err(DispatchError::Busy);
        }
        Ok(VmRun::new(run_id, updates, result, cancellation))
    }

    pub(crate) fn send_input(
        &self,
        run_id: u32,
        input: alloc::string::String,
    ) -> Result<(), ControlError> {
        let slot = self.active_slot(run_id)?;
        if !slot.input_open.get() {
            return Err(ControlError::InputClosed);
        }
        let input = InputMessage::new(input);
        let mut queued = slot.input.borrow_mut();
        if queued.is_some() {
            return Err(ControlError::InputBackpressure);
        }
        *queued = Some(input);
        drop(queued);
        slot.waiting_for_input.set(false);
        slot.wake();
        Ok(())
    }

    pub(crate) fn close_input(&self, run_id: u32) -> Result<(), ControlError> {
        let slot = self.active_slot(run_id)?;
        slot.input_open.set(false);
        slot.waiting_for_input.set(false);
        slot.wake();
        Ok(())
    }

    pub(crate) fn cancel(&self, run_id: u32) -> Result<(), ControlError> {
        let slot = self.active_slot(run_id)?;
        slot.cancel();
        Ok(())
    }

    pub(crate) fn list(&self) -> Vec<VmRunInfo> {
        self.state
            .slots
            .iter()
            .filter(|slot| slot.active.get())
            .map(|slot| VmRunInfo {
                run_id: slot.id.get(),
                state: if slot.waiting_for_input.get() {
                    VmRunState::InputRequired
                } else {
                    VmRunState::Running
                },
            })
            .collect()
    }

    fn active_slot(&self, run_id: u32) -> Result<&RunSlot, ControlError> {
        self.state
            .slots
            .iter()
            .find(|slot| slot.active.get() && slot.id.get() == run_id)
            .ok_or(ControlError::RunNotFound)
    }

    fn reserve_run(&self) -> Result<RunControl, DispatchError> {
        let slot_index = self
            .state
            .slots
            .iter()
            .position(|slot| !slot.active.get())
            .ok_or(DispatchError::Busy)?;
        let mut run_id = self.state.next_run_id.get();
        loop {
            if run_id == 0 {
                run_id = 1;
            }
            if !self
                .state
                .slots
                .iter()
                .any(|slot| slot.active.get() && slot.id.get() == run_id)
            {
                break;
            }
            run_id = run_id.wrapping_add(1);
        }
        self.state.next_run_id.set(run_id.wrapping_add(1));
        let slot = self
            .state
            .slots
            .get(slot_index)
            .ok_or(DispatchError::Busy)?;
        slot.claim(run_id);
        Ok(RunControl {
            state: Rc::clone(&self.state),
            slot_index,
            run_id,
        })
    }
}

pub(crate) struct RunControl {
    state: Rc<RuntimeState>,
    slot_index: usize,
    run_id: u32,
}

impl RunControl {
    fn cancellation(&self) -> RunCancellation {
        RunCancellation {
            state: Rc::clone(&self.state),
            slot_index: self.slot_index,
            run_id: self.run_id,
        }
    }

    fn slot(&self) -> Option<&RunSlot> {
        self.state
            .slots
            .get(self.slot_index)
            .filter(|slot| slot.active.get() && slot.id.get() == self.run_id)
    }

    pub(crate) const fn run_id(&self) -> u32 {
        self.run_id
    }

    pub(crate) fn is_cancelled(&self) -> bool {
        self.slot().is_none_or(|slot| slot.cancelled.get())
    }

    pub(crate) async fn next_input(&self) -> Option<InputMessage> {
        core::future::poll_fn(|context| {
            let Some(slot) = self.slot() else {
                return Poll::Ready(None);
            };
            if slot.cancelled.get() {
                slot.waiting_for_input.set(false);
                return Poll::Ready(None);
            }
            if let Some(input) = slot.input.borrow_mut().take() {
                slot.waiting_for_input.set(false);
                return Poll::Ready(Some(input));
            }
            if !slot.input_open.get() {
                slot.waiting_for_input.set(false);
                return Poll::Ready(None);
            }
            slot.waiting_for_input.set(true);
            let mut waiter = slot.waiter.borrow_mut();
            if waiter
                .as_ref()
                .is_none_or(|registered| !registered.will_wake(context.waker()))
            {
                *waiter = Some(context.waker().clone());
            }
            Poll::Pending
        })
        .await
    }
}

pub(crate) struct RunCancellation {
    state: Rc<RuntimeState>,
    slot_index: usize,
    run_id: u32,
}

impl RunCancellation {
    pub(crate) fn cancel(&self) {
        if let Some(slot) = self
            .state
            .slots
            .get(self.slot_index)
            .filter(|slot| slot.active.get() && slot.id.get() == self.run_id)
        {
            slot.cancel();
        }
    }
}

impl Drop for RunControl {
    fn drop(&mut self) {
        if let Some(slot) = self.state.slots.get(self.slot_index) {
            slot.release(self.run_id);
        }
    }
}

/// Business rejection returned by `vm.run` before a task is accepted.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum DispatchError {
    RuntimeUnavailable,
    Busy,
}

/// Business rejection returned by `vm.input` and `vm.cancel`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ControlError {
    RunNotFound,
    InputBackpressure,
    InputClosed,
}

/// Failure while starting one VM runtime.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum VmRuntimeStartError {
    /// The runtime already owns an Embassy spawner.
    #[error("VM runtime has already started")]
    AlreadyStarted,
}

#[embassy_executor::task(pool_size = VM_TASK_SLOTS)]
async fn vm_execution_task(job: ExecutionJob) {
    execute_run(job).await;
}
