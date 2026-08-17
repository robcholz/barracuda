//! Cooperative owner and driver for loaded Workflow definitions.

use alloc::boxed::Box;
use alloc::collections::VecDeque;
use alloc::rc::Rc;
use alloc::vec::Vec;
use core::cell::{Cell, RefCell};
use core::future::Future;
use core::pin::Pin;
use core::task::{Context, Poll, Waker};

use super::event::EmitRejection;
use super::ingress::{EmitErrorFrame, InternalEmit, InternalEmitFrame, InternalEmitRequest};
use barracuda_rpc::{
    RpcClient, RpcContext, RpcError, RpcFrame, RpcHandler, RpcMulticastBranch, RpcPayloadReader,
    RpcPayloadWriter, RpcResult, RpcStream,
};
use getset::Getters;

use super::{EventId, WorkflowDefinition, WorkflowId, WorkflowLoadError, WorkflowUnloadError};

type WorkflowDriver = Pin<Box<dyn Future<Output = Result<(), WorkflowExecutionError>> + 'static>>;

/// Failure produced while driving one Workflow execution.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum WorkflowExecutionError {
    /// A Workflow RPC transport or runtime operation failed.
    #[error("Workflow RPC step {step} failed: {source}")]
    Rpc {
        /// Zero-based step index.
        step: usize,
        /// Underlying RPC failure.
        #[source]
        source: RpcError,
    },
    /// A Workflow RPC returned its typed Method error.
    #[error("Workflow RPC step {step} returned a Method error")]
    Method {
        /// Zero-based step index.
        step: usize,
    },
    /// One step's response frame cannot be used as the next step's request.
    #[error(
        "Workflow step {from_step} produced a {response_size}-byte frame, but step {to_step} expects {request_size} bytes"
    )]
    InputFrameSizeMismatch {
        /// Step that produced the response frame.
        from_step: usize,
        /// Step receiving the request frame.
        to_step: usize,
        /// Actual response frame size.
        response_size: usize,
        /// Required request frame size.
        request_size: usize,
    },
    /// Event ingress failed after this execution was provisionally created.
    #[error("Event ingress was cancelled before ownership transfer")]
    IngressCancelled,
}

/// Last recorded failure of one fire-and-forget Workflow execution.
#[derive(Clone, Debug, Getters, PartialEq, Eq)]
pub struct WorkflowFailure {
    /// Failed Workflow identity.
    #[getset(get = "pub")]
    workflow_id: WorkflowId,
    /// Execution failure.
    #[getset(get = "pub")]
    error: WorkflowExecutionError,
}

#[derive(Clone)]
struct WorkflowPlan {
    id: WorkflowId,
    steps: Vec<barracuda_rpc::RpcAddress>,
}

struct SharedState {
    definitions: Vec<WorkflowDefinition>,
    pending: VecDeque<WorkflowExecution>,
    runtime_waker: Option<Waker>,
    execution_client: Option<RpcClient>,
}

struct RuntimeShared {
    state: RefCell<SharedState>,
    completed_count: Cell<usize>,
    failed_count: Cell<usize>,
    cancelled_count: Cell<usize>,
    last_failure: RefCell<Option<WorkflowFailure>>,
}

impl RuntimeShared {
    fn definitions(&self) -> Vec<WorkflowDefinition> {
        self.state.borrow().definitions.clone()
    }

    fn contains(&self, workflow_id: &WorkflowId) -> bool {
        self.state
            .borrow()
            .definitions
            .iter()
            .any(|definition| definition.id() == workflow_id)
    }

    fn load(&self, definition: WorkflowDefinition) -> Result<(), WorkflowLoadError> {
        let mut state = self.state.borrow_mut();
        if state
            .definitions
            .iter()
            .any(|loaded| loaded.id() == definition.id())
        {
            return Err(WorkflowLoadError::DuplicateId(definition.id().clone()));
        }
        state.definitions.push(definition);
        Ok(())
    }

    fn unload(&self, workflow_id: &WorkflowId) -> Result<WorkflowDefinition, WorkflowUnloadError> {
        let mut state = self.state.borrow_mut();
        let Some(index) = state
            .definitions
            .iter()
            .position(|definition| definition.id() == workflow_id)
        else {
            return Err(WorkflowUnloadError::NotFound(workflow_id.clone()));
        };
        Ok(state.definitions.remove(index))
    }

    fn matching_plans(&self, event_id: &EventId) -> Vec<WorkflowPlan> {
        self.state
            .borrow()
            .definitions
            .iter()
            .filter(|definition| definition.event().matches(event_id))
            .map(|definition| WorkflowPlan {
                id: definition.id().clone(),
                steps: definition.steps().to_vec(),
            })
            .collect()
    }

    fn enqueue(&self, executions: Vec<WorkflowExecution>) {
        let waker = {
            let mut state = self.state.borrow_mut();
            state.pending.extend(executions);
            state.runtime_waker.take()
        };
        if let Some(waker) = waker {
            waker.wake();
        }
    }

    fn take_pending(&self, waker: &Waker) -> VecDeque<WorkflowExecution> {
        let mut state = self.state.borrow_mut();
        if state
            .runtime_waker
            .as_ref()
            .is_none_or(|registered| !registered.will_wake(waker))
        {
            state.runtime_waker = Some(waker.clone());
        }
        core::mem::take(&mut state.pending)
    }

    fn wake_runtime(&self) {
        let waker = self.state.borrow_mut().runtime_waker.take();
        if let Some(waker) = waker {
            waker.wake();
        }
    }

    fn reset_run_state(&self) {
        let mut state = self.state.borrow_mut();
        state.pending.clear();
        state.runtime_waker = None;
        state.execution_client = None;
    }

    fn set_execution_client(&self, client: RpcClient) {
        self.state.borrow_mut().execution_client = Some(client);
    }

    fn execution_client(&self) -> Option<RpcClient> {
        self.state.borrow().execution_client.clone()
    }
}

/// Immutable snapshot of Workflow execution state.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkflowInfo {
    /// Workflow executions that completed normally.
    pub completed_count: usize,
    /// Workflow executions that failed.
    pub failed_count: usize,
    /// Workflow executions cancelled with Event ingress.
    pub cancelled_count: usize,
    /// Most recently recorded Workflow execution failure.
    pub last_failure: Option<WorkflowFailure>,
}

/// Shared read-only view of a [`WorkflowRuntime`].
#[derive(Clone)]
pub struct WorkflowRuntimeView {
    shared: Rc<RuntimeShared>,
}

/// Shared mutation handle reserved for Event Router's control adapter.
#[derive(Clone)]
pub struct WorkflowRuntimeControl {
    shared: Rc<RuntimeShared>,
}

impl WorkflowRuntimeControl {
    /// Returns whether a Workflow ID is currently loaded.
    #[must_use]
    pub fn contains(&self, workflow_id: &WorkflowId) -> bool {
        self.shared.contains(workflow_id)
    }

    /// Loads a validated Workflow definition.
    pub fn load(&self, definition: WorkflowDefinition) -> Result<(), WorkflowLoadError> {
        self.shared.load(definition)
    }

    /// Unloads one Workflow definition without cancelling running executions.
    pub fn unload(
        &self,
        workflow_id: &WorkflowId,
    ) -> Result<WorkflowDefinition, WorkflowUnloadError> {
        self.shared.unload(workflow_id)
    }
}

impl WorkflowRuntimeView {
    /// Returns loaded Workflow definitions in load order.
    #[must_use]
    pub fn definitions(&self) -> Vec<WorkflowDefinition> {
        self.shared.definitions()
    }

    /// Returns current Workflow execution counters and the latest failure.
    #[must_use]
    pub fn info(&self) -> WorkflowInfo {
        WorkflowInfo {
            completed_count: self.shared.completed_count.get(),
            failed_count: self.shared.failed_count.get(),
            cancelled_count: self.shared.cancelled_count.get(),
            last_failure: self.shared.last_failure.borrow().clone(),
        }
    }
}

/// Runtime owner of loaded definitions and active Workflow executions.
pub struct WorkflowRuntime {
    shared: Rc<RuntimeShared>,
    running: VecDeque<WorkflowExecution>,
}

impl WorkflowRuntime {
    /// Creates an empty Workflow Runtime.
    #[must_use]
    pub fn new() -> Self {
        Self {
            shared: Rc::new(RuntimeShared {
                state: RefCell::new(SharedState {
                    definitions: Vec::new(),
                    pending: VecDeque::new(),
                    runtime_waker: None,
                    execution_client: None,
                }),
                completed_count: Cell::new(0),
                failed_count: Cell::new(0),
                cancelled_count: Cell::new(0),
                last_failure: RefCell::new(None),
            }),
            running: VecDeque::new(),
        }
    }

    /// Creates a read-only view backed by this Runtime's shared state.
    #[must_use]
    pub fn view(&self) -> WorkflowRuntimeView {
        WorkflowRuntimeView {
            shared: Rc::clone(&self.shared),
        }
    }

    /// Creates the mutation handle used by Event Router's control adapter.
    #[must_use]
    pub fn control(&self) -> WorkflowRuntimeControl {
        WorkflowRuntimeControl {
            shared: Rc::clone(&self.shared),
        }
    }

    /// Creates the handler for Event Router's unique Event-ingress RPC.
    pub fn ingress_handler<const M: usize>(&self) -> impl RpcHandler<InternalEmit<M>> + 'static {
        let shared = Rc::clone(&self.shared);
        move |rpc_context: RpcContext, frames: RpcStream<RpcFrame<InternalEmitFrame<M>>>| {
            let shared = Rc::clone(&shared);
            async move { handle_emit::<M>(shared, rpc_context, frames).await }
        }
    }

    /// Makes the Runtime ready to execute Workflows through `client`.
    pub fn start(&mut self, client: RpcClient) {
        self.shared.set_execution_client(client);
    }

    /// Cancels active executions and releases the execution client.
    pub fn stop(&mut self) {
        self.running.clear();
        self.shared.reset_run_state();
    }

    fn poll_running(&mut self, context: &mut Context<'_>) -> Poll<()> {
        self.running
            .extend(self.shared.take_pending(context.waker()));
        let turn_count = self.running.len();
        for _turn in 0..turn_count {
            let Some(mut execution) = self.running.pop_front() else {
                break;
            };
            match execution.poll(context) {
                Poll::Pending => self.running.push_back(execution),
                Poll::Ready(Ok(())) => {
                    self.shared
                        .completed_count
                        .set(self.shared.completed_count.get().saturating_add(1));
                }
                Poll::Ready(Err(WorkflowExecutionError::IngressCancelled)) => {
                    self.shared
                        .cancelled_count
                        .set(self.shared.cancelled_count.get().saturating_add(1));
                }
                Poll::Ready(Err(error)) => {
                    self.shared
                        .failed_count
                        .set(self.shared.failed_count.get().saturating_add(1));
                    self.shared.last_failure.replace(Some(WorkflowFailure {
                        workflow_id: execution.id,
                        error,
                    }));
                }
            }
        }
        Poll::Pending
    }
}

impl Default for WorkflowRuntime {
    fn default() -> Self {
        Self::new()
    }
}

impl Future for WorkflowRuntime {
    type Output = ();

    fn poll(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Self::Output> {
        self.get_mut().poll_running(context)
    }
}

struct WorkflowExecution {
    id: WorkflowId,
    cancellation: Rc<Cell<bool>>,
    drivers: VecDeque<WorkflowDriver>,
}

impl WorkflowExecution {
    fn new(
        plan: WorkflowPlan,
        branch: RpcMulticastBranch,
        client: &RpcClient,
        cancellation: Rc<Cell<bool>>,
    ) -> RpcResult<Self> {
        let mut steps = plan.steps.into_iter();
        let _ingress = steps.next().ok_or(RpcError::InvalidFrameState)?;
        let mut source = branch.into_reader();
        let mut source_step = 0usize;
        let mut drivers: VecDeque<WorkflowDriver> = VecDeque::new();
        for address in steps {
            let destination_step = source_step.saturating_add(1);
            let (writer, reader) = client.call_payload(&address)?;
            drivers.push_back(Box::pin(forward_responses(
                source,
                writer,
                source_step,
                destination_step,
            )));
            source = reader;
            source_step = destination_step;
        }
        drivers.push_back(Box::pin(drain_final_response(source, source_step)));
        Ok(Self {
            id: plan.id,
            cancellation,
            drivers,
        })
    }

    fn poll(&mut self, context: &mut Context<'_>) -> Poll<Result<(), WorkflowExecutionError>> {
        if self.cancellation.get() {
            return Poll::Ready(Err(WorkflowExecutionError::IngressCancelled));
        }
        let turn_count = self.drivers.len();
        for _turn in 0..turn_count {
            let Some(mut driver) = self.drivers.pop_front() else {
                break;
            };
            match driver.as_mut().poll(context) {
                Poll::Ready(Ok(())) => {}
                Poll::Ready(Err(error)) => return Poll::Ready(Err(error)),
                Poll::Pending => self.drivers.push_back(driver),
            }
        }
        if self.drivers.is_empty() {
            Poll::Ready(Ok(()))
        } else {
            Poll::Pending
        }
    }
}

async fn forward_responses(
    mut source: RpcPayloadReader,
    mut destination: RpcPayloadWriter,
    source_step: usize,
    destination_step: usize,
) -> Result<(), WorkflowExecutionError> {
    while let Some(outcome) = source
        .read()
        .await
        .map_err(|source| WorkflowExecutionError::Rpc {
            step: source_step,
            source,
        })?
    {
        let frame = outcome
            .map_err(|_method_error| WorkflowExecutionError::Method { step: source_step })?;
        let response_size = frame.as_ref().len();
        let mut request =
            destination
                .reserve()
                .await
                .map_err(|source| WorkflowExecutionError::Rpc {
                    step: destination_step,
                    source,
                })?;
        let request_size = request.as_mut().len();
        if request_size != response_size {
            return Err(WorkflowExecutionError::InputFrameSizeMismatch {
                from_step: source_step,
                to_step: destination_step,
                response_size,
                request_size,
            });
        }
        request.as_mut().copy_from_slice(frame.as_ref());
        request
            .commit(response_size)
            .map_err(|source| WorkflowExecutionError::Rpc {
                step: destination_step,
                source,
            })?;
    }
    destination
        .close()
        .await
        .map_err(|source| WorkflowExecutionError::Rpc {
            step: destination_step,
            source,
        })
}

async fn drain_final_response(
    mut response: RpcPayloadReader,
    step: usize,
) -> Result<(), WorkflowExecutionError> {
    while let Some(outcome) = response
        .read()
        .await
        .map_err(|source| WorkflowExecutionError::Rpc { step, source })?
    {
        match outcome {
            Ok(frame) => drop(frame),
            Err(_method_error) => return Err(WorkflowExecutionError::Method { step }),
        }
    }
    Ok(())
}

struct DispatchGuard {
    shared: Rc<RuntimeShared>,
    cancellation: Rc<Cell<bool>>,
    accepted: bool,
}

impl DispatchGuard {
    fn new(shared: Rc<RuntimeShared>, cancellation: Rc<Cell<bool>>) -> Self {
        Self {
            shared,
            cancellation,
            accepted: false,
        }
    }

    fn accept(mut self) {
        self.accepted = true;
    }
}

impl Drop for DispatchGuard {
    fn drop(&mut self) {
        if !self.accepted {
            self.cancellation.set(true);
            self.shared.wake_runtime();
        }
    }
}

async fn handle_emit<const M: usize>(
    shared: Rc<RuntimeShared>,
    context: RpcContext,
    frames: RpcStream<RpcFrame<InternalEmitFrame<M>>>,
) -> RpcResult<Result<(), EmitErrorFrame>> {
    let request = match InternalEmitRequest::accept(frames).await? {
        Ok(request) => request,
        Err(rejection) => return Ok(Err(rejection)),
    };
    let plans = shared.matching_plans(request.header().event_id());
    if plans.is_empty() {
        return request.discard().await;
    }
    let Some(execution_client) = shared.execution_client() else {
        return Ok(Err(EmitErrorFrame::new(
            EmitRejection::DownstreamUnavailable,
        )));
    };
    let addresses: Vec<_> = plans
        .iter()
        .filter_map(|plan| plan.steps.first().cloned())
        .collect();
    if addresses.len() != plans.len() {
        return Err(RpcError::InvalidFrameState);
    }
    let (mut writer, branches) = context.client().multicast_payload(&addresses)?;
    if branches.len() != plans.len() {
        return Err(RpcError::InvalidFrameState);
    }

    let cancellation = Rc::new(Cell::new(false));
    let mut executions = Vec::with_capacity(plans.len());
    for (plan, branch) in plans.into_iter().zip(branches) {
        executions.push(WorkflowExecution::new(
            plan,
            branch,
            &execution_client,
            Rc::clone(&cancellation),
        )?);
    }
    let guard = DispatchGuard::new(Rc::clone(&shared), cancellation);
    shared.enqueue(executions);
    let outcome = request.forward_to(&mut writer).await;
    if matches!(outcome, Ok(Ok(()))) {
        guard.accept();
    }
    outcome
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]
    #![allow(missing_docs)]

    use alloc::boxed::Box;
    use alloc::rc::Rc;
    use core::cell::RefCell;
    use core::future::{poll_fn, Future};
    use core::pin::Pin;
    use core::task::Poll;

    use futures_lite::future::block_on;
    use futures_util::stream;

    use super::WorkflowRuntime;
    use crate::{
        EmitError, EmitRejection, Event, EventEmitter, Rule, WorkflowDefinition, WorkflowId,
        WorkflowLoadError, WorkflowUnloadError,
    };
    use barracuda_rpc::{
        RpcAddress, RpcFrame, RpcLaneStorage, RpcMethod, RpcRegistry, RpcStream, Streaming, Unary,
    };

    use super::InternalEmit;

    fn rule(value: &str) -> Rule {
        Rule::try_from(value).expect("valid test Rule")
    }

    fn address(value: &str) -> RpcAddress {
        RpcAddress::try_from(value).expect("valid test RPC address")
    }

    fn definition(id: &str, event: &str, steps: &[&str]) -> WorkflowDefinition {
        WorkflowDefinition::new(
            WorkflowId::try_from(id).expect("valid test Workflow ID"),
            rule(event),
            steps.iter().map(|step| address(step)).collect(),
        )
        .expect("valid test Workflow definition")
    }

    fn register_runtime<const N: usize, const M: usize, const Q: usize>(
        runtime: &WorkflowRuntime,
        registry: &RpcRegistry<N, M, Q>,
    ) {
        registry
            .register::<InternalEmit<M>, _>(runtime.ingress_handler::<M>())
            .expect("register Workflow Runtime ingress");
    }

    #[test]
    fn runtime_view_preserves_loaded_workflow_order() {
        let runtime = WorkflowRuntime::new();
        let view = runtime.view();
        let control = runtime.control();
        control
            .load(definition(
                "broad",
                "gateway.*",
                &["adapter.broad", "agent.run"],
            ))
            .expect("load broad Workflow");
        control
            .load(definition(
                "exact",
                "gateway.message.received",
                &["adapter.exact"],
            ))
            .expect("load exact Workflow");
        control
            .load(definition(
                "scheduler",
                "scheduler.*",
                &["adapter.scheduler"],
            ))
            .expect("load scheduler Workflow");

        let definitions = view.definitions();
        let ids: alloc::vec::Vec<_> = definitions.iter().map(|item| item.id().as_str()).collect();
        assert_eq!(ids, ["broad", "exact", "scheduler"]);
    }

    #[test]
    fn runtime_control_rejects_duplicate_ids_and_missing_unloads() {
        let runtime = WorkflowRuntime::new();
        let control = runtime.control();
        control
            .load(definition("gateway", "gateway.*", &["adapter.gateway"]))
            .expect("load Workflow");

        let duplicate = control.load(definition("gateway", "scheduler.*", &["adapter.scheduler"]));
        assert!(matches!(duplicate, Err(WorkflowLoadError::DuplicateId(_))));

        control
            .unload(&WorkflowId::try_from("gateway").expect("valid Workflow ID"))
            .expect("unload Workflow");
        assert!(runtime.view().definitions().is_empty());
        assert!(matches!(
            control.unload(&WorkflowId::try_from("gateway").expect("valid Workflow ID")),
            Err(WorkflowUnloadError::NotFound(_))
        ));
    }

    struct RuntimeEvent;

    impl Event for RuntimeEvent {
        const ID: &'static str = "runtime.event";
        type Message = [u8; 4];
        type Input = Unary;
    }

    struct AddOne;

    impl RpcMethod for AddOne {
        const ADDRESS: &'static str = "workflow-test.add-one";
        type Request = [u8; 4];
        type Response = [u8; 4];
        type Error = [u8; 1];
        type Input = Unary;
        type Output = Unary;
    }

    struct Record;

    impl RpcMethod for Record {
        const ADDRESS: &'static str = "workflow-test.record";
        type Request = [u8; 4];
        type Response = ();
        type Error = [u8; 1];
        type Input = Unary;
        type Output = Unary;
    }

    struct RecordAlpha;

    impl RpcMethod for RecordAlpha {
        const ADDRESS: &'static str = "workflow-test.record-alpha";
        type Request = [u8; 4];
        type Response = ();
        type Error = [u8; 1];
        type Input = Unary;
        type Output = Unary;
    }

    struct RecordBeta;

    impl RpcMethod for RecordBeta {
        const ADDRESS: &'static str = "workflow-test.record-beta";
        type Request = [u8; 4];
        type Response = ();
        type Error = [u8; 1];
        type Input = Unary;
        type Output = Unary;
    }

    struct Reject;

    impl RpcMethod for Reject {
        const ADDRESS: &'static str = "workflow-test.reject";
        type Request = [u8; 4];
        type Response = ();
        type Error = [u8; 1];
        type Input = Unary;
        type Output = Unary;
    }

    struct Expand;

    impl RpcMethod for Expand {
        const ADDRESS: &'static str = "workflow-test.expand";
        type Request = [u8; 1];
        type Response = [u8; 1];
        type Error = [u8; 1];
        type Input = Unary;
        type Output = Streaming;
    }

    struct Collect;

    impl RpcMethod for Collect {
        const ADDRESS: &'static str = "workflow-test.collect";
        type Request = [u8; 1];
        type Response = ();
        type Error = [u8; 1];
        type Input = Streaming;
        type Output = Unary;
    }

    struct ByteEvent;

    impl Event for ByteEvent {
        const ID: &'static str = "runtime.bytes";
        type Message = [u8; 1];
        type Input = Unary;
    }

    #[test]
    fn runtime_polls_running_workflow_through_every_rpc_step() {
        const FRAME_SIZE: usize = 256;

        let lanes = Box::leak(Box::new(RpcLaneStorage::<4, FRAME_SIZE, 4>::new()));
        let registry = RpcRegistry::new(lanes);
        let recorded = Rc::new(RefCell::new(None));

        registry
            .register::<AddOne, _>(|_context, request: RpcFrame<[u8; 4]>| async move {
                let value = u32::from_le_bytes(*request.view()?);
                Ok(Ok(value.saturating_add(1).to_le_bytes()))
            })
            .expect("register first Workflow step");
        let handler_recorded = Rc::clone(&recorded);
        registry
            .register::<Record, _>(move |_context, request: RpcFrame<[u8; 4]>| {
                let recorded = Rc::clone(&handler_recorded);
                async move {
                    recorded.replace(Some(u32::from_le_bytes(*request.view()?)));
                    Ok(Ok(()))
                }
            })
            .expect("register final Workflow step");

        let mut runtime = WorkflowRuntime::new();
        let view = runtime.view();
        runtime
            .control()
            .load(definition(
                "increment",
                RuntimeEvent::ID,
                &[AddOne::ADDRESS, Record::ADDRESS],
            ))
            .expect("load Workflow");
        register_runtime(&runtime, &registry);
        runtime.start(registry.client());
        let emitter = EventEmitter::<FRAME_SIZE>::new(registry.client());
        let mut emit = Box::pin(emitter.emit::<RuntimeEvent>(41_u32.to_le_bytes()));
        let mut emit_complete = false;
        let mut polls_after_delivery = 0usize;
        block_on(poll_fn(|context| {
            assert!(Pin::new(&mut runtime).poll(context).is_pending());
            if !emit_complete {
                if let Poll::Ready(result) = Pin::new(&mut emit).poll(context) {
                    result.expect("emit Event");
                    emit_complete = true;
                }
            }
            let delivered = emit_complete && recorded.borrow().as_ref() == Some(&42);
            if delivered {
                polls_after_delivery = polls_after_delivery.saturating_add(1);
            }
            if polls_after_delivery >= 2 {
                return Poll::Ready(());
            }
            if delivered {
                context.waker().wake_by_ref();
            }
            Poll::Pending
        }));
        assert_eq!(recorded.take(), Some(42));
        let info = view.info();
        assert_eq!(info.completed_count, 1);
        assert_eq!(info.failed_count, 0);
    }

    #[test]
    fn runtime_polls_every_workflow_matched_by_one_event() {
        const FRAME_SIZE: usize = 256;

        let lanes = Box::leak(Box::new(RpcLaneStorage::<3, FRAME_SIZE, 3>::new()));
        let registry = RpcRegistry::new(lanes);
        let alpha = Rc::new(RefCell::new(None));
        let beta = Rc::new(RefCell::new(None));

        let handler_alpha = Rc::clone(&alpha);
        registry
            .register::<RecordAlpha, _>(move |_context, request: RpcFrame<[u8; 4]>| {
                let alpha = Rc::clone(&handler_alpha);
                async move {
                    alpha.replace(Some(*request.view()?));
                    Ok(Ok(()))
                }
            })
            .expect("register alpha Workflow");
        let handler_beta = Rc::clone(&beta);
        registry
            .register::<RecordBeta, _>(move |_context, request: RpcFrame<[u8; 4]>| {
                let beta = Rc::clone(&handler_beta);
                async move {
                    beta.replace(Some(*request.view()?));
                    Ok(Ok(()))
                }
            })
            .expect("register beta Workflow");

        let mut runtime = WorkflowRuntime::new();
        runtime
            .control()
            .load(definition("alpha", "runtime.*", &[RecordAlpha::ADDRESS]))
            .expect("load alpha Workflow");
        runtime
            .control()
            .load(definition("beta", RuntimeEvent::ID, &[RecordBeta::ADDRESS]))
            .expect("load beta Workflow");
        register_runtime(&runtime, &registry);
        runtime.start(registry.client());
        let emitter = EventEmitter::<FRAME_SIZE>::new(registry.client());
        let input = 7_u32.to_le_bytes();
        let mut emit = Box::pin(emitter.emit::<RuntimeEvent>(input));
        let mut emit_complete = false;
        let mut polls_after_delivery = 0usize;
        block_on(poll_fn(|context| {
            assert!(Pin::new(&mut runtime).poll(context).is_pending());
            if !emit_complete {
                if let Poll::Ready(result) = Pin::new(&mut emit).poll(context) {
                    result.expect("emit Event");
                    emit_complete = true;
                }
            }
            let delivered = emit_complete
                && alpha.borrow().as_ref() == Some(&input)
                && beta.borrow().as_ref() == Some(&input);
            if delivered {
                polls_after_delivery = polls_after_delivery.saturating_add(1);
            }
            if polls_after_delivery >= 2 {
                return Poll::Ready(());
            }
            if delivered {
                context.waker().wake_by_ref();
            }
            Poll::Pending
        }));
        let info = runtime.view().info();
        assert_eq!(info.completed_count, 2);
        assert_eq!(info.failed_count, 0);
    }

    #[test]
    fn workflow_method_error_is_recorded_after_emitter_acceptance() {
        const FRAME_SIZE: usize = 256;

        let lanes = Box::leak(Box::new(RpcLaneStorage::<2, FRAME_SIZE, 2>::new()));
        let registry = RpcRegistry::new(lanes);
        registry
            .register::<Reject, _>(
                |_context, _request: RpcFrame<[u8; 4]>| async move { Ok(Err([9])) },
            )
            .expect("register rejecting Workflow step");

        let mut runtime = WorkflowRuntime::new();
        runtime
            .control()
            .load(definition("reject", RuntimeEvent::ID, &[Reject::ADDRESS]))
            .expect("load rejecting Workflow");
        register_runtime(&runtime, &registry);
        runtime.start(registry.client());
        let emitter = EventEmitter::<FRAME_SIZE>::new(registry.client());
        let mut emit = Box::pin(emitter.emit::<RuntimeEvent>(1_u32.to_le_bytes()));
        let mut emit_complete = false;
        let mut polls_after_emit = 0usize;
        block_on(poll_fn(|context| {
            assert!(Pin::new(&mut runtime).poll(context).is_pending());
            if !emit_complete {
                if let Poll::Ready(result) = Pin::new(&mut emit).poll(context) {
                    result.expect("Emitter only observes ingress acceptance");
                    emit_complete = true;
                    context.waker().wake_by_ref();
                }
                return Poll::Pending;
            }
            polls_after_emit = polls_after_emit.saturating_add(1);
            if polls_after_emit >= 2 {
                Poll::Ready(())
            } else {
                context.waker().wake_by_ref();
                Poll::Pending
            }
        }));
        let info = runtime.view().info();
        assert_eq!(info.completed_count, 0);
        assert_eq!(info.failed_count, 1);
        let failure = info.last_failure.expect("recorded Workflow failure");
        assert_eq!(failure.workflow_id().as_str(), "reject");
        assert!(matches!(
            failure.error(),
            super::WorkflowExecutionError::Method { step: 0 }
        ));
    }

    #[test]
    fn unmatched_event_is_consumed_without_starting_a_workflow() {
        const FRAME_SIZE: usize = 64;

        let lanes = Box::leak(Box::new(RpcLaneStorage::<1, FRAME_SIZE, 1>::new()));
        let registry = RpcRegistry::new(lanes);
        let mut runtime = WorkflowRuntime::new();
        register_runtime(&runtime, &registry);
        runtime.start(registry.client());
        let emitter = EventEmitter::<FRAME_SIZE>::new(registry.client());
        block_on(async {
            let mut emit = Box::pin(emitter.emit::<RuntimeEvent>(5_u32.to_le_bytes()));
            poll_fn(|context| {
                assert!(Pin::new(&mut runtime).poll(context).is_pending());
                Pin::new(&mut emit).poll(context)
            })
            .await
            .expect("discard unmatched Event");
        });
        let info = runtime.view().info();
        assert_eq!(info.completed_count, 0);
        assert_eq!(info.failed_count, 0);
        assert_eq!(info.cancelled_count, 0);
    }

    #[test]
    fn runtime_streams_multiple_response_frames_into_the_next_step() {
        const FRAME_SIZE: usize = 64;

        let lanes = Box::leak(Box::new(RpcLaneStorage::<3, FRAME_SIZE, 3>::new()));
        let registry = RpcRegistry::new(lanes);
        let collected = Rc::new(RefCell::new(alloc::vec::Vec::new()));

        registry
            .register::<Expand, _>(|_context, request: RpcFrame<[u8; 1]>| async move {
                let [first] = *request.view()?;
                Ok(RpcStream::new(stream::iter([
                    Ok(Ok([first])),
                    Ok(Ok([first.saturating_add(1)])),
                    Ok(Ok([first.saturating_add(2)])),
                ])))
            })
            .expect("register expanding Workflow step");
        let handler_collected = Rc::clone(&collected);
        registry
            .register::<Collect, _>(
                move |_context, mut requests: RpcStream<RpcFrame<[u8; 1]>>| {
                    let collected = Rc::clone(&handler_collected);
                    async move {
                        while let Some(request) = requests.next().await {
                            let request = request?;
                            let [value] = *request.view()?;
                            collected.borrow_mut().push(value);
                        }
                        Ok(Ok(()))
                    }
                },
            )
            .expect("register collecting Workflow step");

        let mut runtime = WorkflowRuntime::new();
        runtime
            .control()
            .load(definition(
                "stream",
                ByteEvent::ID,
                &[Expand::ADDRESS, Collect::ADDRESS],
            ))
            .expect("load streaming Workflow");
        register_runtime(&runtime, &registry);
        runtime.start(registry.client());
        let emitter = EventEmitter::<FRAME_SIZE>::new(registry.client());
        let mut emit = Box::pin(emitter.emit::<ByteEvent>([10]));
        let mut emit_complete = false;
        let mut polls_after_delivery = 0usize;
        block_on(poll_fn(|context| {
            assert!(Pin::new(&mut runtime).poll(context).is_pending());
            if !emit_complete {
                if let Poll::Ready(result) = Pin::new(&mut emit).poll(context) {
                    result.expect("emit streaming Workflow input");
                    emit_complete = true;
                }
            }
            let delivered = emit_complete && collected.borrow().as_slice() == [10, 11, 12];
            if delivered {
                polls_after_delivery = polls_after_delivery.saturating_add(1);
            }
            if polls_after_delivery >= 2 {
                return Poll::Ready(());
            }
            if delivered {
                context.waker().wake_by_ref();
            }
            Poll::Pending
        }));
        assert_eq!(collected.borrow().as_slice(), [10, 11, 12]);
        let info = runtime.view().info();
        assert_eq!(info.completed_count, 1);
        assert_eq!(info.failed_count, 0);
    }

    #[test]
    fn matching_event_is_rejected_until_workflow_runtime_is_running() {
        const FRAME_SIZE: usize = 64;

        let lanes = Box::leak(Box::new(RpcLaneStorage::<1, FRAME_SIZE, 1>::new()));
        let registry = RpcRegistry::new(lanes);
        let runtime = WorkflowRuntime::new();
        runtime
            .control()
            .load(definition("waiting", RuntimeEvent::ID, &[Record::ADDRESS]))
            .expect("load Workflow");
        register_runtime(&runtime, &registry);

        let error = block_on(
            EventEmitter::<FRAME_SIZE>::new(registry.client())
                .emit::<RuntimeEvent>(1_u32.to_le_bytes()),
        )
        .expect_err("Runtime has not started polling");
        assert!(matches!(
            error,
            EmitError::Rejected(EmitRejection::DownstreamUnavailable)
        ));
    }
}
