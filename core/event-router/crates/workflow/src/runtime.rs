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
use super::link::{classify, FieldRef, LinkKind};
use barracuda_rpc::{
    RpcAddress, RpcCardinality, RpcClient, RpcContext, RpcError, RpcFrame, RpcHandler,
    RpcMethodInfo, RpcMulticastBranch, RpcPayloadReader, RpcPayloadWriter, RpcResult, RpcStream,
};
use getset::Getters;
use serde_json::Value;

use super::{
    EventId, Topic, WorkflowControlRejection, WorkflowDefinition, WorkflowId, WorkflowLoadError,
    WorkflowStep, WorkflowUnloadError,
};

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
    /// A `Link::Direct` edge joined two steps whose response and request types
    /// are not the identical fixed-layout type.
    #[error(
        "Workflow step {from_step} response type does not match step {to_step} request type"
    )]
    LinkTypeMismatch {
        /// Step producing the response.
        from_step: usize,
        /// Step receiving the request.
        to_step: usize,
    },
    /// A link joined a streaming response to a unary request. Only unary→unary,
    /// streaming→streaming, and unary→streaming links are valid.
    #[error(
        "Workflow step {from_step} streams its response but step {to_step} accepts a single request"
    )]
    LinkCardinalityMismatch {
        /// Step producing the response.
        from_step: usize,
        /// Step receiving the request.
        to_step: usize,
    },
    /// A mapping or literal step's predecessor produced no response frame.
    #[error("Workflow step {step} produced no response for the next step to map")]
    MissingResponse {
        /// Step that produced no response.
        step: usize,
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
    steps: Vec<WorkflowStep>,
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

    fn matching_plans(&self, event_id: &EventId, topic: Option<&Topic>) -> Vec<WorkflowPlan> {
        self.state
            .borrow()
            .definitions
            .iter()
            .filter(|definition| {
                definition.event().matches(event_id)
                    && definition
                        .topic()
                        .is_none_or(|required| topic == Some(required))
            })
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
        // Resolve every step's method projection up front, surfacing missing
        // endpoints before any IO starts.
        let mut infos: Vec<RpcMethodInfo> = Vec::with_capacity(plan.steps.len());
        for step in &plan.steps {
            infos.push(client.method_info(step.address())?);
        }

        // Validate every link once, before wiring any downstream RPC. A bad link
        // never invokes its target: the execution drains the ingress response so
        // the producer completes, then reports the failure.
        if let Err(error) = validate_links(&plan.steps, &infos) {
            let mut drivers: VecDeque<WorkflowDriver> = VecDeque::new();
            drivers.push_back(Box::pin(report_link_error(branch.into_reader(), error)));
            return Ok(Self {
                id: plan.id,
                cancellation,
                drivers,
            });
        }

        // Step 0 is invoked by the multicast with the Event payload; its
        // response is the source for step 1.
        let mut prev_info = infos.first().cloned().ok_or(RpcError::InvalidFrameState)?;
        let mut source = branch.into_reader();
        let mut source_step = 0usize;
        let mut drivers: VecDeque<WorkflowDriver> = VecDeque::new();

        for index in 1..plan.steps.len() {
            let step = plan.steps.get(index).ok_or(RpcError::InvalidFrameState)?;
            let this_info = infos.get(index).cloned().ok_or(RpcError::InvalidFrameState)?;
            let destination_step = index;
            let (writer, reader) = client.call_payload(step.address())?;
            // Grammar was validated at load time; an error here is unexpected.
            let kind = classify(step.arguments()).map_err(|_error| RpcError::InvalidFrameState)?;
            let transform = match kind {
                LinkKind::Direct => LinkTransform::Direct,
                LinkKind::Literal { arguments } => LinkTransform::Literal(Box::new(
                    LiteralTransform {
                        info: this_info.clone(),
                        address: step.address().clone(),
                        arguments,
                        destination_step,
                    },
                )),
                LinkKind::Mapping {
                    arguments,
                    references,
                } => LinkTransform::Mapping(Box::new(MappingTransform {
                    prev_info: prev_info.clone(),
                    this_info: this_info.clone(),
                    address: step.address().clone(),
                    arguments,
                    references,
                    source_step,
                    destination_step,
                })),
            };
            drivers.push_back(Box::pin(drive(
                source,
                writer,
                source_step,
                destination_step,
                transform,
            )));
            source = reader;
            source_step = destination_step;
            prev_info = this_info;
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

/// The per-frame request transform of one link — the link kind.
///
/// Frame flow is the driver's concern; a transform only produces the request
/// bytes for one response frame. `Direct` is a byte-for-byte passthrough,
/// `Literal` a constant request from the arguments, and `Mapping` literal
/// arguments plus wire-copied reference fields.
enum LinkTransform {
    /// Byte-for-byte passthrough of the response frame.
    Direct,
    /// Constant request built solely from the literal arguments.
    Literal(Box<LiteralTransform>),
    /// Literal arguments plus wire-copied reference fields.
    Mapping(Box<MappingTransform>),
}

/// Everything a literal transform needs to assemble its constant request.
struct LiteralTransform {
    /// Destination method projection.
    info: RpcMethodInfo,
    /// Destination address.
    address: RpcAddress,
    /// Literal arguments object.
    arguments: Value,
    /// Destination step index.
    destination_step: usize,
}

/// Everything a mapping transform needs to assemble one request.
struct MappingTransform {
    /// Source method projection.
    prev_info: RpcMethodInfo,
    /// Destination method projection.
    this_info: RpcMethodInfo,
    /// Destination address.
    address: RpcAddress,
    /// Literal arguments object (references removed).
    arguments: Value,
    /// Fields copied out of the source response.
    references: Vec<FieldRef>,
    /// Source step index.
    source_step: usize,
    /// Destination step index.
    destination_step: usize,
}

impl LinkTransform {
    /// Produces this step's request bytes for one source response frame.
    fn apply(&mut self, frame: &[u8], request: &mut [u8]) -> Result<usize, WorkflowExecutionError> {
        match self {
            LinkTransform::Direct => {
                let size = frame.len();
                request
                    .get_mut(..size)
                    .ok_or_else(|| step_error(0, RpcError::InvalidFrameState))?
                    .copy_from_slice(frame);
                Ok(size)
            }
            LinkTransform::Literal(step) => {
                let encoded = step
                    .info
                    .encode_request(&step.address, &step.arguments)
                    .map_err(|source| step_error(step.destination_step, source))?;
                copy_request(request, &encoded)
            }
            LinkTransform::Mapping(step) => {
                let mut encoded = step
                    .this_info
                    .encode_request(&step.address, &step.arguments)
                    .map_err(|source| step_error(step.destination_step, source))?;
                for field in &step.references {
                    let source_wire = step.prev_info.wire().ok_or_else(|| {
                        step_error(step.source_step, RpcError::NotJsonCallable(step.address.clone()))
                    })?;
                    let dest_wire = step.this_info.wire().ok_or_else(|| {
                        step_error(
                            step.destination_step,
                            RpcError::NotJsonCallable(step.address.clone()),
                        )
                    })?;
                    let value = source_wire
                        .read_response_field(frame, &field.source_field)
                        .map_err(|source| step_error(step.source_step, source))?;
                    dest_wire
                        .write_request_field(&mut encoded, &field.dest_field, value)
                        .map_err(|source| step_error(step.destination_step, source))?;
                }
                copy_request(request, &encoded)
            }
        }
    }
}

fn copy_request(request: &mut [u8], encoded: &[u8]) -> Result<usize, WorkflowExecutionError> {
    let size = encoded.len();
    request
        .get_mut(..size)
        .ok_or_else(|| step_error(0, RpcError::InvalidFrameState))?
        .copy_from_slice(encoded);
    Ok(size)
}

fn step_error(step: usize, source: RpcError) -> WorkflowExecutionError {
    WorkflowExecutionError::Rpc { step, source }
}

/// Drives one link between two steps.
///
/// Frame flow is this layer's concern: every response frame of the source
/// step is read, transformed by the link kind, and written to the destination
/// step; source EOF closes the destination. Unary and streaming sources flow
/// through the same loop — cardinality rules are enforced by link validation,
/// not here.
async fn drive(
    mut source: RpcPayloadReader,
    mut destination: RpcPayloadWriter,
    source_step: usize,
    destination_step: usize,
    mut transform: LinkTransform,
) -> Result<(), WorkflowExecutionError> {
    while let Some(outcome) = source
        .read()
        .await
        .map_err(|source| step_error(source_step, source))?
    {
        let frame = outcome
            .map_err(|_method_error| WorkflowExecutionError::Method { step: source_step })?;
        let mut reservation = destination
            .reserve()
            .await
            .map_err(|source| step_error(destination_step, source))?;
        let request_size = transform.apply(frame.as_ref(), reservation.as_mut())?;
        drop(frame);
        if request_size != reservation.as_mut().len() {
            return Err(WorkflowExecutionError::InputFrameSizeMismatch {
                from_step: source_step,
                to_step: destination_step,
                response_size: request_size,
                request_size: reservation.as_mut().len(),
            });
        }
        reservation
            .commit(request_size)
            .map_err(|source| step_error(destination_step, source))?;
    }
    destination
        .close()
        .await
        .map_err(|source| step_error(destination_step, source))
}

/// Validates every step link against the resolved method projections.
///
/// Run once at execution setup — the closest point to load time that has a
/// client. A Direct edge must join identical types; a Mapping edge's every
/// reference must resolve to an existing field on both sides with the source no
/// larger than the destination; a Literal or Mapping edge's target must be
/// runtime-dynamic. Any failure aborts the execution before a downstream RPC is
/// invoked.
fn validate_links(
    steps: &[WorkflowStep],
    infos: &[RpcMethodInfo],
) -> Result<(), WorkflowExecutionError> {
    for index in 1..steps.len() {
        let from_step = index.saturating_sub(1);
        let to_step = index;
        let prev = infos.get(from_step).ok_or(internal_error(to_step))?;
        let this = infos.get(index).ok_or(internal_error(to_step))?;
        let prev_step = steps.get(from_step).ok_or(internal_error(to_step))?;
        let step = steps.get(index).ok_or(internal_error(to_step))?;
        let kind =
            classify(step.arguments()).map_err(|_error| internal_error(to_step))?;
        // Only unary→unary, streaming→streaming, and unary→streaming links are
        // valid; a streaming response feeding a unary request is rejected.
        if prev.output_mode() == RpcCardinality::Streaming
            && this.input_mode() == RpcCardinality::Unary
        {
            return Err(WorkflowExecutionError::LinkCardinalityMismatch { from_step, to_step });
        }
        match kind {
            LinkKind::Direct => {
                if prev.descriptor().response_type_id() != this.descriptor().request_type_id() {
                    return Err(WorkflowExecutionError::LinkTypeMismatch { from_step, to_step });
                }
            }
            LinkKind::Literal { arguments } => {
                if this.wire().is_none() {
                    return Err(not_dynamic(to_step, step.address().clone()));
                }
                validate_arguments(this, step, to_step, &arguments)?;
            }
            LinkKind::Mapping {
                arguments,
                references,
            } => {
                let prev_wire = prev
                    .wire()
                    .ok_or_else(|| not_dynamic(from_step, prev_step.address().clone()))?;
                let this_wire = this
                    .wire()
                    .ok_or_else(|| not_dynamic(to_step, step.address().clone()))?;
                for reference in &references {
                    let source_size = prev_wire
                        .response_field_size(&reference.source_field)
                        .ok_or_else(|| unknown_field(from_step, reference.source_field.clone()))?;
                    let dest_size = this_wire
                        .request_field_size(&reference.dest_field)
                        .ok_or_else(|| unknown_field(to_step, reference.dest_field.clone()))?;
                    if source_size > dest_size {
                        return Err(WorkflowExecutionError::Rpc {
                            step: to_step,
                            source: RpcError::WireFieldTooLarge {
                                field: reference.dest_field.clone(),
                                size: source_size,
                                capacity: dest_size,
                            },
                        });
                    }
                }
                validate_arguments(this, step, to_step, &arguments)?;
            }
        }
    }
    Ok(())
}

/// Dry-runs the literal arguments through the destination's JSON codec so a
/// request that cannot be assembled is rejected before any IO.
fn validate_arguments(
    this: &RpcMethodInfo,
    step: &WorkflowStep,
    to_step: usize,
    arguments: &Value,
) -> Result<(), WorkflowExecutionError> {
    this.encode_request(step.address(), arguments)
        .map_err(|source| WorkflowExecutionError::Rpc {
            step: to_step,
            source,
        })?;
    Ok(())
}

/// Validates every step link of `definition` against the methods currently
/// registered with `client`.
///
/// This is the primary gate for `workflow.load`: addresses must resolve,
/// Direct edges must be type-identical, Mapping/Literal targets must be
/// runtime-dynamic, references must resolve with the source no larger than
/// the destination, cardinality combos must be valid, and the literal
/// arguments must transcode into the request. It shares its rule set with
/// the per-execution validation, which remains as a backstop for startup
/// restore and dynamic registry changes.
///
/// # Errors
///
/// Returns [`WorkflowControlRejection::UnknownMethod`] when a step addresses
/// an unregistered method, or [`WorkflowControlRejection::InvalidLink`] when
/// any link rule is violated.
pub fn validate_definition(
    client: &RpcClient,
    definition: &WorkflowDefinition,
) -> Result<(), WorkflowControlRejection> {
    let mut infos = Vec::with_capacity(definition.steps().len());
    for step in definition.steps() {
        let info = client
            .method_info(step.address())
            .map_err(|_error| WorkflowControlRejection::UnknownMethod)?;
        infos.push(info);
    }
    validate_links(definition.steps(), &infos).map_err(|_error| WorkflowControlRejection::InvalidLink)
}

fn internal_error(step: usize) -> WorkflowExecutionError {
    WorkflowExecutionError::Rpc {
        step,
        source: RpcError::InvalidFrameState,
    }
}

fn not_dynamic(step: usize, address: RpcAddress) -> WorkflowExecutionError {
    WorkflowExecutionError::Rpc {
        step,
        source: RpcError::NotJsonCallable(address),
    }
}

fn unknown_field(step: usize, field: alloc::string::String) -> WorkflowExecutionError {
    WorkflowExecutionError::Rpc {
        step,
        source: RpcError::WireFieldUnknown { field },
    }
}

/// Drains the ingress response so the producer completes, then reports a link
/// validation failure without invoking any downstream RPC.
async fn report_link_error(
    mut source: RpcPayloadReader,
    error: WorkflowExecutionError,
) -> Result<(), WorkflowExecutionError> {
    while let Ok(Some(_outcome)) = source.read().await {}
    Err(error)
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
    let plans = shared.matching_plans(request.header().event_id(), request.header().topic());
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
        .filter_map(|plan| plan.steps.first().map(|step| step.address().clone()))
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
        EmitError, EmitRejection, Event, EventEmitter, EventId, Rule, Topic, WorkflowDefinition,
        WorkflowId, WorkflowLoadError, WorkflowStep, WorkflowUnloadError,
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
            steps
                .iter()
                .map(|step| WorkflowStep::new(address(step), None))
                .collect(),
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

    #[test]
    fn runtime_matches_event_and_optional_topic_together() {
        let runtime = WorkflowRuntime::new();
        let control = runtime.control();
        control
            .load(definition("broad", "scheduler.triggered", &["alarm.all"]))
            .expect("load topic-independent Workflow");
        control
            .load(
                WorkflowDefinition::with_topic(
                    WorkflowId::try_from("morning").expect("valid Workflow ID"),
                    rule("scheduler.triggered"),
                    Topic::try_from("morning.weekday").expect("valid topic"),
                    alloc::vec![WorkflowStep::new(address("alarm.morning"), None)],
                )
                .expect("valid topic Workflow"),
            )
            .expect("load topic Workflow");

        let event = EventId::try_from("scheduler.triggered").expect("valid Event ID");
        let morning = Topic::try_from("morning.weekday").expect("valid topic");
        let night = Topic::try_from("night.weekday").expect("valid topic");

        assert_eq!(runtime.shared.matching_plans(&event, None).len(), 1);
        assert_eq!(
            runtime.shared.matching_plans(&event, Some(&morning)).len(),
            2
        );
        assert_eq!(runtime.shared.matching_plans(&event, Some(&night)).len(), 1);
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
            .load(
                WorkflowDefinition::with_topic(
                    WorkflowId::try_from("increment").expect("valid Workflow ID"),
                    rule(RuntimeEvent::ID),
                    Topic::try_from("runtime-1").expect("valid topic"),
                    alloc::vec![
                        WorkflowStep::new(address(AddOne::ADDRESS), None),
                        WorkflowStep::new(address(Record::ADDRESS), None),
                    ],
                )
                .expect("valid topic Workflow"),
            )
            .expect("load Workflow");
        register_runtime(&runtime, &registry);
        runtime.start(registry.client());
        let emitter = EventEmitter::<FRAME_SIZE>::new(registry.client());
        let topic = Topic::try_from("runtime-1").expect("valid topic");
        let mut emit = Box::pin(emitter.emit_to::<RuntimeEvent>(&topic, 41_u32.to_le_bytes()));
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

#[cfg(test)]
mod link_tests {
    #![allow(clippy::expect_used)]
    #![allow(missing_docs)]

    use alloc::boxed::Box;
    use alloc::rc::Rc;
    use alloc::vec;
    use alloc::vec::Vec;
    use core::cell::RefCell;
    use core::future::{poll_fn, Future};
    use core::pin::Pin;
    use core::task::Poll;

    use barracuda_rpc::{
        rpc_dynamic, RpcAddress, RpcFrame, RpcLaneStorage, RpcMethod, RpcRegistry, RpcStream,
        RpcWire, Streaming, Unary,
    };
    use futures_lite::future::block_on;
    use futures_util::stream;
    use serde::{Deserialize, Serialize};
    use serde_json::json;
    use zerocopy::{Immutable, IntoBytes, KnownLayout, TryFromBytes};

    use super::{InternalEmit, WorkflowRuntime};
    use crate::{
        validate_definition, Event, EventEmitter, Rule, WorkflowControlRejection, WorkflowDefinition,
        WorkflowExecutionError, WorkflowId, WorkflowStep,
    };

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

    struct Produce;

    #[rpc_dynamic]
    impl RpcMethod for Produce {
        const ADDRESS: &'static str = "linktest.produce";
        type Request = Seed;
        type Response = Reply;
        type Error = ();
        type Input = Unary;
        type Output = Unary;
    }

    struct Consume;

    #[rpc_dynamic]
    impl RpcMethod for Consume {
        const ADDRESS: &'static str = "linktest.consume";
        type Request = Deliver;
        type Response = ();
        type Error = ();
        type Input = Unary;
        type Output = Unary;
    }

    // Same 4-byte layout as `Reply`, but a distinct type: a Direct link from
    // `Produce` (Response = Reply) into this must be rejected.
    struct ConsumeSeed;

    #[rpc_dynamic]
    impl RpcMethod for ConsumeSeed {
        const ADDRESS: &'static str = "linktest.consume-seed";
        type Request = Seed;
        type Response = ();
        type Error = ();
        type Input = Unary;
        type Output = Unary;
    }

    // Streams `Reply` frames: a Direct link into a unary sink passes type
    // identity but must be rejected on cardinality (streaming → unary).
    struct StreamSource;

    #[rpc_dynamic]
    impl RpcMethod for StreamSource {
        const ADDRESS: &'static str = "linktest.stream-source";
        type Request = Seed;
        type Response = Reply;
        type Error = ();
        type Input = Unary;
        type Output = Streaming;
    }

    struct StreamSink;

    impl RpcMethod for StreamSink {
        const ADDRESS: &'static str = "linktest.stream-sink";
        type Request = Reply;
        type Response = ();
        type Error = ();
        type Input = Unary;
        type Output = Unary;
    }

    // Streaming-input consumer of mapping requests.
    struct ConsumeStream;

    #[rpc_dynamic]
    impl RpcMethod for ConsumeStream {
        const ADDRESS: &'static str = "linktest.consume-stream";
        type Request = Deliver;
        type Response = ();
        type Error = ();
        type Input = Streaming;
        type Output = Unary;
    }

    struct SeedEvent;

    impl Event for SeedEvent {
        const ID: &'static str = "linktest.seed";
        type Message = Seed;
        type Input = Unary;
    }

    fn address(value: &str) -> RpcAddress {
        RpcAddress::try_from(value).expect("valid test RPC address")
    }

    fn drive_until_settled<const N: usize, const M: usize, const Q: usize>(
        runtime: &mut WorkflowRuntime,
        registry: &RpcRegistry<N, M, Q>,
        input: Seed,
        ready: impl Fn() -> bool,
    ) {
        let emitter = EventEmitter::<M>::new(registry.client());
        let mut emit = Box::pin(emitter.emit::<SeedEvent>(input));
        let mut emit_complete = false;
        let mut settled_polls = 0usize;
        block_on(poll_fn(|context| {
            assert!(Pin::new(&mut *runtime).poll(context).is_pending());
            if !emit_complete {
                if let Poll::Ready(result) = Pin::new(&mut emit).poll(context) {
                    result.expect("emit Event");
                    emit_complete = true;
                }
            }
            if emit_complete && ready() {
                settled_polls = settled_polls.saturating_add(1);
            }
            if settled_polls >= 2 {
                return Poll::Ready(());
            }
            if emit_complete {
                context.waker().wake_by_ref();
            }
            Poll::Pending
        }));
    }

    #[test]
    fn mapping_link_copies_a_referenced_field_into_the_next_request() {
        const FRAME_SIZE: usize = 256;

        let lanes = Box::leak(Box::new(RpcLaneStorage::<4, FRAME_SIZE, 4>::new()));
        let registry = RpcRegistry::new(lanes);
        let recorded = Rc::new(RefCell::new(None));

        registry
            .register::<Produce, _>(|_context, request: RpcFrame<Seed>| async move {
                let seed = *request.view()?;
                Ok(Ok(Reply { token: seed.n }))
            })
            .expect("register produce");
        let handler_recorded = Rc::clone(&recorded);
        registry
            .register::<Consume, _>(move |_context, request: RpcFrame<Deliver>| {
                let recorded = Rc::clone(&handler_recorded);
                async move {
                    recorded.replace(Some(*request.view()?));
                    Ok(Ok(()))
                }
            })
            .expect("register consume");

        let mut runtime = WorkflowRuntime::new();
        let definition = WorkflowDefinition::new(
            WorkflowId::try_from("mapping").expect("valid Workflow ID"),
            Rule::try_from(SeedEvent::ID).expect("valid rule"),
            vec![
                WorkflowStep::new(address(Produce::ADDRESS), None),
                WorkflowStep::new(
                    address(Consume::ADDRESS),
                    Some(json!({ "extra": 5, "token": "$previous.output.token" })),
                ),
            ],
        )
        .expect("valid mapping Workflow");
        runtime.control().load(definition).expect("load Workflow");
        registry
            .register::<InternalEmit<FRAME_SIZE>, _>(runtime.ingress_handler::<FRAME_SIZE>())
            .expect("register ingress");
        runtime.start(registry.client());

        let recorded_probe = Rc::clone(&recorded);
        drive_until_settled(&mut runtime, &registry, Seed { n: 41 }, move || {
            recorded_probe.borrow().is_some()
        });

        assert_eq!(recorded.take(), Some(Deliver { token: 41, extra: 5 }));
        let info = runtime.view().info();
        assert_eq!(info.completed_count, 1);
        assert_eq!(info.failed_count, 0);
    }

    #[test]
    fn direct_link_between_distinct_types_is_rejected_before_io() {
        const FRAME_SIZE: usize = 256;

        let lanes = Box::leak(Box::new(RpcLaneStorage::<4, FRAME_SIZE, 4>::new()));
        let registry = RpcRegistry::new(lanes);
        let touched = Rc::new(RefCell::new(false));

        registry
            .register::<Produce, _>(|_context, request: RpcFrame<Seed>| async move {
                let seed = *request.view()?;
                Ok(Ok(Reply { token: seed.n }))
            })
            .expect("register produce");
        let handler_touched = Rc::clone(&touched);
        registry
            .register::<ConsumeSeed, _>(move |_context, _request: RpcFrame<Seed>| {
                let touched = Rc::clone(&handler_touched);
                async move {
                    touched.replace(true);
                    Ok(Ok(()))
                }
            })
            .expect("register consume-seed");

        let mut runtime = WorkflowRuntime::new();
        // No arguments -> Direct link. Reply and Seed share a size but are
        // different types, so type identity must reject the passthrough.
        let definition = WorkflowDefinition::new(
            WorkflowId::try_from("mismatch").expect("valid Workflow ID"),
            Rule::try_from(SeedEvent::ID).expect("valid rule"),
            vec![
                WorkflowStep::new(address(Produce::ADDRESS), None),
                WorkflowStep::new(address(ConsumeSeed::ADDRESS), None),
            ],
        )
        .expect("valid Workflow");
        runtime.control().load(definition).expect("load Workflow");
        registry
            .register::<InternalEmit<FRAME_SIZE>, _>(runtime.ingress_handler::<FRAME_SIZE>())
            .expect("register ingress");
        runtime.start(registry.client());

        let runtime_probe = runtime.view();
        drive_until_settled(&mut runtime, &registry, Seed { n: 3 }, move || {
            runtime_probe.info().failed_count == 1
        });

        assert!(!*touched.borrow(), "mismatched step must never be invoked");
        let failure = runtime
            .view()
            .info()
            .last_failure
            .expect("recorded failure");
        assert!(matches!(
            failure.error(),
            WorkflowExecutionError::LinkTypeMismatch {
                from_step: 0,
                to_step: 1,
            }
        ));
    }

    #[test]
    fn validate_definition_rejects_unresolvable_and_invalid_links_at_load_time() {
        const FRAME_SIZE: usize = 256;

        let lanes = Box::leak(Box::new(RpcLaneStorage::<4, FRAME_SIZE, 4>::new()));
        let registry = RpcRegistry::new(lanes);
        registry
            .register::<Produce, _>(|_context, request: RpcFrame<Seed>| async move {
                let seed = *request.view()?;
                Ok(Ok(Reply { token: seed.n }))
            })
            .expect("register produce");
        registry
            .register::<Consume, _>(|_context, _request: RpcFrame<Deliver>| async move {
                Ok(Ok(()))
            })
            .expect("register consume");
        registry
            .register::<ConsumeSeed, _>(|_context, _request: RpcFrame<Seed>| async move {
                Ok(Ok(()))
            })
            .expect("register consume-seed");
        let client = registry.client();

        let valid = WorkflowDefinition::new(
            WorkflowId::try_from("valid").expect("valid Workflow ID"),
            Rule::try_from(SeedEvent::ID).expect("valid rule"),
            vec![
                WorkflowStep::new(address(Produce::ADDRESS), None),
                WorkflowStep::new(
                    address(Consume::ADDRESS),
                    Some(json!({ "extra": 5, "token": "$previous.output.token" })),
                ),
            ],
        )
        .expect("valid Workflow");
        validate_definition(&client, &valid).expect("valid links pass");

        let unknown = WorkflowDefinition::new(
            WorkflowId::try_from("unknown").expect("valid Workflow ID"),
            Rule::try_from(SeedEvent::ID).expect("valid rule"),
            vec![
                WorkflowStep::new(address(Produce::ADDRESS), None),
                WorkflowStep::new(address("linktest.missing"), None),
            ],
        )
        .expect("valid Workflow");
        assert_eq!(
            validate_definition(&client, &unknown),
            Err(WorkflowControlRejection::UnknownMethod)
        );

        let mismatched = WorkflowDefinition::new(
            WorkflowId::try_from("mismatched").expect("valid Workflow ID"),
            Rule::try_from(SeedEvent::ID).expect("valid rule"),
            vec![
                WorkflowStep::new(address(Produce::ADDRESS), None),
                WorkflowStep::new(address(ConsumeSeed::ADDRESS), None),
            ],
        )
        .expect("valid Workflow");
        assert_eq!(
            validate_definition(&client, &mismatched),
            Err(WorkflowControlRejection::InvalidLink)
        );

        let bad_arguments = WorkflowDefinition::new(
            WorkflowId::try_from("bad-arguments").expect("valid Workflow ID"),
            Rule::try_from(SeedEvent::ID).expect("valid rule"),
            vec![
                WorkflowStep::new(address(Produce::ADDRESS), None),
                WorkflowStep::new(
                    address(Consume::ADDRESS),
                    Some(json!({ "extra": "not-a-number" })),
                ),
            ],
        )
        .expect("valid Workflow");
        assert_eq!(
            validate_definition(&client, &bad_arguments),
            Err(WorkflowControlRejection::InvalidLink)
        );
    }

    #[test]
    fn mapping_link_streams_each_response_frame_through_the_transform() {
        const FRAME_SIZE: usize = 256;

        let lanes = Box::leak(Box::new(RpcLaneStorage::<4, FRAME_SIZE, 4>::new()));
        let registry = RpcRegistry::new(lanes);
        let collected = Rc::new(RefCell::new(Vec::new()));

        registry
            .register::<StreamSource, _>(|_context, request: RpcFrame<Seed>| async move {
                let seed = *request.view()?;
                Ok(RpcStream::new(stream::iter([
                    Ok(Ok(Reply { token: seed.n })),
                    Ok(Ok(Reply { token: seed.n.saturating_add(1) })),
                ])))
            })
            .expect("register stream source");
        let handler_collected = Rc::clone(&collected);
        registry
            .register::<ConsumeStream, _>(
                move |_context, mut requests: RpcStream<RpcFrame<Deliver>>| {
                    let collected = Rc::clone(&handler_collected);
                    async move {
                        while let Some(request) = requests.next().await {
                            collected.borrow_mut().push(*request?.view()?);
                        }
                        Ok(Ok(()))
                    }
                },
            )
            .expect("register consume stream");

        let mut runtime = WorkflowRuntime::new();
        let definition = WorkflowDefinition::new(
            WorkflowId::try_from("mapping-stream").expect("valid Workflow ID"),
            Rule::try_from(SeedEvent::ID).expect("valid rule"),
            vec![
                WorkflowStep::new(address(StreamSource::ADDRESS), None),
                WorkflowStep::new(
                    address(ConsumeStream::ADDRESS),
                    Some(json!({ "extra": 5, "token": "$previous.output.token" })),
                ),
            ],
        )
        .expect("valid mapping Workflow");
        runtime.control().load(definition).expect("load Workflow");
        registry
            .register::<InternalEmit<FRAME_SIZE>, _>(runtime.ingress_handler::<FRAME_SIZE>())
            .expect("register ingress");
        runtime.start(registry.client());

        let collected_probe = Rc::clone(&collected);
        drive_until_settled(&mut runtime, &registry, Seed { n: 41 }, move || {
            collected_probe.borrow().len() == 2
        });

        assert_eq!(
            collected.take().as_slice(),
            &[
                Deliver { token: 41, extra: 5 },
                Deliver { token: 42, extra: 5 },
            ]
        );
        let info = runtime.view().info();
        assert_eq!(info.completed_count, 1);
        assert_eq!(info.failed_count, 0);
    }

    #[test]
    fn mapping_link_feeds_a_unary_response_into_a_streaming_request() {
        const FRAME_SIZE: usize = 256;

        let lanes = Box::leak(Box::new(RpcLaneStorage::<4, FRAME_SIZE, 4>::new()));
        let registry = RpcRegistry::new(lanes);
        let collected = Rc::new(RefCell::new(Vec::new()));

        registry
            .register::<Produce, _>(|_context, request: RpcFrame<Seed>| async move {
                let seed = *request.view()?;
                Ok(Ok(Reply { token: seed.n }))
            })
            .expect("register produce");
        let handler_collected = Rc::clone(&collected);
        registry
            .register::<ConsumeStream, _>(
                move |_context, mut requests: RpcStream<RpcFrame<Deliver>>| {
                    let collected = Rc::clone(&handler_collected);
                    async move {
                        while let Some(request) = requests.next().await {
                            collected.borrow_mut().push(*request?.view()?);
                        }
                        Ok(Ok(()))
                    }
                },
            )
            .expect("register consume stream");

        let mut runtime = WorkflowRuntime::new();
        let definition = WorkflowDefinition::new(
            WorkflowId::try_from("mapping-u-to-s").expect("valid Workflow ID"),
            Rule::try_from(SeedEvent::ID).expect("valid rule"),
            vec![
                WorkflowStep::new(address(Produce::ADDRESS), None),
                WorkflowStep::new(
                    address(ConsumeStream::ADDRESS),
                    Some(json!({ "extra": 7, "token": "$previous.output.token" })),
                ),
            ],
        )
        .expect("valid mapping Workflow");
        runtime.control().load(definition).expect("load Workflow");
        registry
            .register::<InternalEmit<FRAME_SIZE>, _>(runtime.ingress_handler::<FRAME_SIZE>())
            .expect("register ingress");
        runtime.start(registry.client());

        let collected_probe = Rc::clone(&collected);
        drive_until_settled(&mut runtime, &registry, Seed { n: 9 }, move || {
            collected_probe.borrow().len() == 1
        });

        assert_eq!(
            collected.take().as_slice(),
            &[Deliver { token: 9, extra: 7 }]
        );
        let info = runtime.view().info();
        assert_eq!(info.completed_count, 1);
        assert_eq!(info.failed_count, 0);
    }

    #[test]
    fn streaming_response_into_unary_request_is_rejected_before_io() {
        const FRAME_SIZE: usize = 256;

        let lanes = Box::leak(Box::new(RpcLaneStorage::<4, FRAME_SIZE, 4>::new()));
        let registry = RpcRegistry::new(lanes);
        let touched = Rc::new(RefCell::new(false));

        registry
            .register::<StreamSource, _>(|_context, request: RpcFrame<Seed>| async move {
                let seed = *request.view()?;
                Ok(RpcStream::new(stream::iter([Ok(Ok(Reply { token: seed.n }))])))
            })
            .expect("register stream source");
        let handler_touched = Rc::clone(&touched);
        registry
            .register::<StreamSink, _>(move |_context, _request: RpcFrame<Reply>| {
                let touched = Rc::clone(&handler_touched);
                async move {
                    touched.replace(true);
                    Ok(Ok(()))
                }
            })
            .expect("register stream sink");

        let mut runtime = WorkflowRuntime::new();
        // No arguments -> Direct link. Reply feeds Reply (type identity holds),
        // but the source streams its response into a unary sink: cardinality
        // must reject the edge.
        let definition = WorkflowDefinition::new(
            WorkflowId::try_from("cardinality").expect("valid Workflow ID"),
            Rule::try_from(SeedEvent::ID).expect("valid rule"),
            vec![
                WorkflowStep::new(address(StreamSource::ADDRESS), None),
                WorkflowStep::new(address(StreamSink::ADDRESS), None),
            ],
        )
        .expect("valid Workflow");
        runtime.control().load(definition).expect("load Workflow");
        registry
            .register::<InternalEmit<FRAME_SIZE>, _>(runtime.ingress_handler::<FRAME_SIZE>())
            .expect("register ingress");
        runtime.start(registry.client());

        let runtime_probe = runtime.view();
        drive_until_settled(&mut runtime, &registry, Seed { n: 3 }, move || {
            runtime_probe.info().failed_count == 1
        });

        assert!(!*touched.borrow(), "cardinality-mismatched step must never be invoked");
        let failure = runtime
            .view()
            .info()
            .last_failure
            .expect("recorded failure");
        assert!(matches!(
            failure.error(),
            WorkflowExecutionError::LinkCardinalityMismatch {
                from_step: 0,
                to_step: 1,
            }
        ));
    }
}
