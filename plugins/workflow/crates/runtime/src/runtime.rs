//! Cooperative owner and driver for loaded Workflow definitions.

use alloc::boxed::Box;
use alloc::collections::VecDeque;
use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec::Vec;
use core::cell::{Cell, RefCell};
use core::future::Future;
use core::pin::Pin;
use core::task::{Context, Poll, Waker};

use getset::Getters;
use serde_json::{Map, Value};

use crate::action::ErasedWorkflowAction;
use crate::definition::{WorkflowCondition, WorkflowOperation};
use crate::link::{FieldRef, LinkKind, SourceSelector};
use crate::{
    EmitError, Event, EventId, JsonText, Topic, WorkflowActionRegistry, WorkflowControlRejection,
    WorkflowDefinition, WorkflowId, WorkflowLoadError, WorkflowUnloadError, WorkflowValue,
};

/// Executions one Event may have queued or running before emitters that
/// wait with [`WorkflowRuntimeControl::ready_for`] pause.
pub const EVENT_BACKLOG_LIMIT: usize = 4;

type WorkflowDriver = Pin<Box<dyn Future<Output = Result<(), WorkflowExecutionError>> + 'static>>;

/// Failure produced while driving one Workflow execution.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum WorkflowExecutionError {
    /// A direct Action failed.
    #[error("Workflow Action step {step} failed: {source}")]
    Action {
        /// Zero-based step index.
        step: usize,
        /// Action-provided failure.
        #[source]
        source: crate::WorkflowActionError,
    },
    /// An Action used by the loaded definition was unavailable at dispatch.
    #[error("Workflow Action step {step} is not registered")]
    UnknownAction {
        /// Zero-based step index.
        step: usize,
    },
    /// Adjacent JSON schemas do not support the requested link.
    #[error("Workflow JSON link from step {from_step} to step {to_step} is incompatible")]
    InvalidLink {
        /// Step producing the JSON response.
        from_step: usize,
        /// Step receiving the JSON request.
        to_step: usize,
    },
    /// A Mapping link requires the previous output to be a JSON object.
    #[error("Workflow Action step {step} output is not an object")]
    ResponseNotObject {
        /// Step that produced the non-object response.
        step: usize,
    },
    /// A Mapping link referenced a field absent from the actual output.
    #[error("Workflow Action step {step} output omitted field {field}")]
    MissingOutputField {
        /// Step that produced the response.
        step: usize,
        /// Missing top-level response field.
        field: String,
    },
    /// A Mapping link requires the Event input to be a JSON object.
    #[error("Workflow Event input is not an object")]
    EventInputNotObject,
    /// A Mapping link referenced a field absent from the Event input.
    #[error("Workflow Event input omitted field {field}")]
    MissingEventInputField {
        /// Missing top-level Event input field.
        field: String,
    },
    /// A step selected a previous output that does not exist.
    #[error("Workflow Action step {step} has no previous output")]
    PreviousOutputUnavailable {
        /// Step whose arguments contain the invalid selector.
        step: usize,
    },
    /// Bulk memory could not hold a step's request.
    #[error("Workflow Action step {step} request does not fit in memory")]
    OutOfMemory {
        /// Step whose request could not be built.
        step: usize,
    },
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
    definition: Rc<WorkflowDefinition>,
}

struct SharedState {
    definitions: Vec<Rc<WorkflowDefinition>>,
    pending: VecDeque<WorkflowExecution>,
    runtime_waker: Option<Waker>,
    /// Queued or running executions per triggering Event.
    backlog: Vec<(EventId, usize)>,
    /// Emitters waiting for an Event's backlog to drain.
    backlog_waiters: Vec<Waker>,
}

struct RuntimeShared {
    state: RefCell<SharedState>,
    actions: WorkflowActionRegistry,
    completed_count: Cell<usize>,
    failed_count: Cell<usize>,
    last_failure: RefCell<Option<WorkflowFailure>>,
}

impl RuntimeShared {
    fn definitions(&self) -> Vec<WorkflowDefinition> {
        self.state
            .borrow()
            .definitions
            .iter()
            .map(|definition| definition.as_ref().clone())
            .collect()
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
        state.definitions.push(Rc::new(definition));
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
        let definition = state.definitions.remove(index);
        Ok(Rc::try_unwrap(definition).unwrap_or_else(|definition| definition.as_ref().clone()))
    }

    fn has_listener(&self, event_id: &str) -> bool {
        self.state.borrow().definitions.iter().any(|definition| {
            definition.event().matches_str(event_id) && definition.topic().is_none()
        })
    }

    fn backlog(&self, event_id: &str) -> usize {
        self.state
            .borrow()
            .backlog
            .iter()
            .find(|(id, _count)| id.as_str() == event_id)
            .map_or(0, |(_id, count)| *count)
    }

    fn poll_ready_for(&self, event_id: &str, context: &mut Context<'_>) -> Poll<()> {
        if self.backlog(event_id) < EVENT_BACKLOG_LIMIT {
            return Poll::Ready(());
        }
        let mut state = self.state.borrow_mut();
        if !state
            .backlog_waiters
            .iter()
            .any(|waiter| waiter.will_wake(context.waker()))
        {
            state.backlog_waiters.push(context.waker().clone());
        }
        Poll::Pending
    }

    fn finish(&self, event_id: &EventId) {
        let waiters = {
            let mut state = self.state.borrow_mut();
            let Some(index) = state.backlog.iter().position(|(id, _count)| id == event_id) else {
                return;
            };
            let remaining = state.backlog.get_mut(index).map_or(0, |(_id, count)| {
                *count = count.saturating_sub(1);
                *count
            });
            if remaining == 0 {
                state.backlog.swap_remove(index);
            }
            if remaining >= EVENT_BACKLOG_LIMIT {
                return;
            }
            core::mem::take(&mut state.backlog_waiters)
        };
        for waiter in waiters {
            waiter.wake();
        }
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
                definition: Rc::clone(definition),
            })
            .collect()
    }

    fn dispatch(&self, event_id: &EventId, topic: Option<&Topic>, input: impl Into<JsonText>) {
        let plans = self.matching_plans(event_id, topic);
        log::debug!(
            "Workflow Event `{}` matched {} definition(s)",
            event_id.as_str(),
            plans.len()
        );
        if plans.is_empty() {
            return;
        }
        // Encoded once; every matching execution shares it.
        let input = input.into();
        let mut executions = VecDeque::new();
        for plan in plans {
            match WorkflowExecution::new(plan, event_id, input.clone(), &self.actions) {
                Ok(execution) => executions.push_back(execution),
                Err((workflow_id, error)) => self.record_failure(workflow_id, error),
            }
        }
        if executions.is_empty() {
            return;
        }
        let waker = {
            let mut state = self.state.borrow_mut();
            match state.backlog.iter_mut().find(|(id, _count)| id == event_id) {
                Some((_id, count)) => *count = count.saturating_add(executions.len()),
                None => state.backlog.push((event_id.clone(), executions.len())),
            }
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

    fn record_failure(&self, workflow_id: WorkflowId, error: WorkflowExecutionError) {
        self.failed_count
            .set(self.failed_count.get().saturating_add(1));
        self.last_failure
            .replace(Some(WorkflowFailure { workflow_id, error }));
    }
}

/// Immutable snapshot of Workflow execution state.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkflowInfo {
    /// Workflow executions that completed normally.
    pub completed_count: usize,
    /// Workflow executions that failed.
    pub failed_count: usize,
    /// Most recently recorded Workflow execution failure.
    pub last_failure: Option<WorkflowFailure>,
}

/// Shared read-only view of a [`WorkflowRuntime`].
#[derive(Clone)]
pub struct WorkflowRuntimeView {
    shared: Rc<RuntimeShared>,
}

/// Shared mutation and Event-emission handle for a [`WorkflowRuntime`].
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

    /// Unloads a definition without cancelling running execution snapshots.
    pub fn unload(
        &self,
        workflow_id: &WorkflowId,
    ) -> Result<WorkflowDefinition, WorkflowUnloadError> {
        self.shared.unload(workflow_id)
    }

    /// Returns whether a loaded Workflow would run for an untopiced `E`.
    ///
    /// Emitters check this before building an Event they would otherwise
    /// discard.
    #[must_use]
    pub fn has_listener<E>(&self) -> bool
    where
        E: Event,
    {
        self.shared.has_listener(E::ID)
    }

    /// Waits until fewer than [`EVENT_BACKLOG_LIMIT`] executions started by
    /// `E` are queued or running.
    ///
    /// A high-rate emitter awaits this between Events so its backlog stays
    /// bounded instead of queuing every Event while the Runtime catches up.
    pub async fn ready_for<E>(&self)
    where
        E: Event,
    {
        core::future::poll_fn(|context| self.shared.poll_ready_for(E::ID, context)).await;
    }

    /// Emits one typed Event directly into the Workflow Runtime.
    pub fn emit<E>(&self, input: impl Into<JsonText>) -> Result<(), EmitError>
    where
        E: Event,
    {
        self.emit_inner::<E>(None, input)
    }

    /// Emits one typed Event with a topic filter.
    pub fn emit_to<E>(&self, topic: Topic, input: impl Into<JsonText>) -> Result<(), EmitError>
    where
        E: Event,
    {
        self.emit_inner::<E>(Some(topic), input)
    }

    fn emit_inner<E>(
        &self,
        topic: Option<Topic>,
        input: impl Into<JsonText>,
    ) -> Result<(), EmitError>
    where
        E: Event,
    {
        let event_id = EventId::try_from(E::ID)?;
        self.shared.dispatch(&event_id, topic.as_ref(), input);
        Ok(())
    }

    /// Emits a dynamically identified Event.
    pub fn emit_event(&self, event_id: EventId, topic: Option<Topic>, input: impl Into<JsonText>) {
        self.shared.dispatch(&event_id, topic.as_ref(), input);
    }
}

impl WorkflowRuntimeView {
    /// Returns loaded Workflow definitions in load order.
    #[must_use]
    pub fn definitions(&self) -> Vec<WorkflowDefinition> {
        self.shared.definitions()
    }

    /// Returns current execution counters and the latest failure.
    #[must_use]
    pub fn info(&self) -> WorkflowInfo {
        WorkflowInfo {
            completed_count: self.shared.completed_count.get(),
            failed_count: self.shared.failed_count.get(),
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
    /// Creates an empty Runtime that resolves steps through `actions`.
    #[must_use]
    pub fn new(actions: WorkflowActionRegistry) -> Self {
        Self {
            shared: Rc::new(RuntimeShared {
                state: RefCell::new(SharedState {
                    definitions: Vec::new(),
                    pending: VecDeque::new(),
                    runtime_waker: None,
                    backlog: Vec::new(),
                    backlog_waiters: Vec::new(),
                }),
                actions,
                completed_count: Cell::new(0),
                failed_count: Cell::new(0),
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

    /// Creates the mutation and Event-emission handle.
    #[must_use]
    pub fn control(&self) -> WorkflowRuntimeControl {
        WorkflowRuntimeControl {
            shared: Rc::clone(&self.shared),
        }
    }

    fn poll_running(&mut self, context: &mut Context<'_>) -> Poll<()> {
        self.running
            .extend(self.shared.take_pending(context.waker()));
        let turn_count = self.running.len();
        for _turn in 0..turn_count {
            let Some(mut execution) = self.running.pop_front() else {
                break;
            };
            match execution.driver.as_mut().poll(context) {
                Poll::Pending => self.running.push_back(execution),
                Poll::Ready(Ok(())) => {
                    log::debug!("Workflow `{}` completed", execution.id.as_str());
                    self.shared
                        .completed_count
                        .set(self.shared.completed_count.get().saturating_add(1));
                    self.shared.finish(&execution.event);
                }
                Poll::Ready(Err(error)) => {
                    log::error!("Workflow `{}` failed: {error}", execution.id.as_str());
                    self.shared.finish(&execution.event);
                    self.shared.record_failure(execution.id, error);
                }
            }
        }
        Poll::Pending
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
    /// The Event that started this execution, for its backlog count.
    event: EventId,
    driver: WorkflowDriver,
}

impl WorkflowExecution {
    fn new(
        plan: WorkflowPlan,
        event_id: &EventId,
        event: JsonText,
        registry: &WorkflowActionRegistry,
    ) -> Result<Self, (WorkflowId, WorkflowExecutionError)> {
        let id = plan.definition.id().clone();
        let actions = match resolve_actions(registry, &plan.definition) {
            Ok(actions) => actions,
            Err(error) => return Err((id, error)),
        };
        if !plan.definition.has_branch() {
            if let Err(error) = validate_links(&plan.definition, &actions) {
                return Err((id, error));
            }
        }
        let definition = Rc::clone(&plan.definition);
        let driver = Box::pin(async move { execute_steps(&definition, &event, &actions).await });
        Ok(Self {
            id,
            event: event_id.clone(),
            driver,
        })
    }
}

async fn execute_steps(
    definition: &WorkflowDefinition,
    event_input: &JsonText,
    actions: &[Rc<dyn ErasedWorkflowAction>],
) -> Result<(), WorkflowExecutionError> {
    let mut frames = Vec::new();
    frames.push(OperationFrame {
        operations: definition.operations(),
        cursor: 0,
    });
    let mut previous = None;
    let mut previous_step = None;
    while let Some(frame) = frames.last_mut() {
        let Some(operation) = frame.operations.get(frame.cursor) else {
            frames.pop();
            continue;
        };
        frame.cursor = frame.cursor.saturating_add(1);
        match operation {
            WorkflowOperation::Call(to_step) => {
                previous = Some(
                    execute_call(
                        definition,
                        *to_step,
                        event_input,
                        actions,
                        previous.take(),
                        previous_step,
                    )
                    .await?,
                );
                previous_step = Some(*to_step);
            }
            WorkflowOperation::Return => return Ok(()),
            WorkflowOperation::Branch(branch) => {
                let selected = evaluate_condition(
                    &branch.condition,
                    event_input,
                    previous.as_ref(),
                    previous_step,
                )?;
                frames.push(OperationFrame {
                    operations: if selected {
                        &branch.then_operations
                    } else {
                        &branch.else_operations
                    },
                    cursor: 0,
                });
            }
        }
    }
    Ok(())
}

struct OperationFrame<'a> {
    operations: &'a [WorkflowOperation],
    cursor: usize,
}

async fn execute_call(
    definition: &WorkflowDefinition,
    to_step: usize,
    event_input: &JsonText,
    actions: &[Rc<dyn ErasedWorkflowAction>],
    previous: Option<JsonText>,
    _previous_step: Option<usize>,
) -> Result<JsonText, WorkflowExecutionError> {
    let _step = definition
        .steps()
        .get(to_step)
        .ok_or(WorkflowExecutionError::UnknownAction { step: to_step })?;
    let link = definition
        .links()
        .get(to_step)
        .ok_or(WorkflowExecutionError::UnknownAction { step: to_step })?;
    let request = match link {
        LinkKind::Direct => previous.unwrap_or_else(|| event_input.clone()),
        LinkKind::Literal { arguments } => JsonText::try_from_value(arguments)
            .map_err(|_error| WorkflowExecutionError::OutOfMemory { step: to_step })?,
        LinkKind::Mapping {
            arguments,
            references,
        } => mapped_request(
            event_input,
            previous.as_ref(),
            to_step,
            arguments,
            references,
        )?,
    };
    let action = actions
        .get(to_step)
        .ok_or(WorkflowExecutionError::UnknownAction { step: to_step })?;
    action
        .invoke_erased(request)
        .await
        .map_err(|source| WorkflowExecutionError::Action {
            step: to_step,
            source,
        })
}

fn evaluate_condition(
    condition: &WorkflowCondition,
    event_input: &JsonText,
    previous: Option<&JsonText>,
    previous_step: Option<usize>,
) -> Result<bool, WorkflowExecutionError> {
    let source = match condition.selector {
        SourceSelector::EventInput => event_input,
        SourceSelector::PreviousOutput => {
            previous.ok_or(WorkflowExecutionError::PreviousOutputUnavailable {
                step: previous_step.unwrap_or(0),
            })?
        }
    };
    // Only the compared value is decoded.
    let actual = match &condition.field {
        None => source.to_value(),
        Some(field) => {
            if !source.is_object() {
                return Err(match condition.selector {
                    SourceSelector::EventInput => WorkflowExecutionError::EventInputNotObject,
                    SourceSelector::PreviousOutput => WorkflowExecutionError::ResponseNotObject {
                        step: previous_step.unwrap_or(0),
                    },
                });
            }
            source
                .field(field)
                .map_or(Value::Null, |actual| actual.to_value())
        }
    };
    Ok(condition.comparison.matches(&actual))
}

/// Builds a step request from literal arguments and referenced values.
///
/// Referenced values are spliced in as their stored text, so a large field
/// selected from the Event input is copied once into the new request rather
/// than decoded and re-encoded.
fn mapped_request(
    event_input: &JsonText,
    previous: Option<&JsonText>,
    to_step: usize,
    arguments: &WorkflowValue,
    references: &[FieldRef],
) -> Result<JsonText, WorkflowExecutionError> {
    let literal = arguments
        .as_object()
        .ok_or(WorkflowExecutionError::InvalidLink {
            from_step: to_step.saturating_sub(1),
            to_step,
        })?;
    let mut values = Vec::with_capacity(references.len());
    for reference in references {
        let source = match reference.selector {
            SourceSelector::EventInput => event_input,
            SourceSelector::PreviousOutput => previous
                .ok_or(WorkflowExecutionError::PreviousOutputUnavailable { step: to_step })?,
        };
        let value = match &reference.source_field {
            None => source.clone(),
            Some(field) => {
                if !source.is_object() {
                    return Err(match reference.selector {
                        SourceSelector::EventInput => WorkflowExecutionError::EventInputNotObject,
                        SourceSelector::PreviousOutput => {
                            WorkflowExecutionError::ResponseNotObject {
                                step: to_step.saturating_sub(1),
                            }
                        }
                    });
                }
                source
                    .field(field)
                    .ok_or_else(|| match reference.selector {
                        SourceSelector::EventInput => {
                            WorkflowExecutionError::MissingEventInputField {
                                field: field.clone(),
                            }
                        }
                        SourceSelector::PreviousOutput => {
                            WorkflowExecutionError::MissingOutputField {
                                step: to_step.saturating_sub(1),
                                field: field.clone(),
                            }
                        }
                    })?
            }
        };
        values.push(value);
    }
    JsonText::try_encode_object(|object| {
        for (name, value) in literal {
            object.value(name, value);
        }
        for (reference, value) in references.iter().zip(&values) {
            object
                .field(&reference.dest_field)
                .put(value.as_str().as_bytes());
        }
    })
    .map_err(|_error| WorkflowExecutionError::OutOfMemory { step: to_step })
}

fn resolve_actions(
    registry: &WorkflowActionRegistry,
    definition: &WorkflowDefinition,
) -> Result<Vec<Rc<dyn ErasedWorkflowAction>>, WorkflowExecutionError> {
    definition
        .steps()
        .iter()
        .enumerate()
        .map(|(step, definition)| {
            registry
                .resolve(definition.address())
                .ok_or(WorkflowExecutionError::UnknownAction { step })
        })
        .collect()
}

fn schema_properties(schema: &Value) -> Option<&Map<String, Value>> {
    schema.get("properties")?.as_object()
}

/// Request and response schemas of one step, parsed only for link validation.
struct StepShapes {
    request: Value,
    response: Value,
}

fn validate_links(
    definition: &WorkflowDefinition,
    actions: &[Rc<dyn ErasedWorkflowAction>],
) -> Result<(), WorkflowExecutionError> {
    let descriptors = actions
        .iter()
        .enumerate()
        .map(|(step, action)| {
            let descriptor = action.descriptor();
            let invalid = |_error| WorkflowExecutionError::InvalidLink {
                from_step: step.saturating_sub(1),
                to_step: step,
            };
            Ok(StepShapes {
                request: descriptor.request_shape().map_err(invalid)?,
                response: descriptor.response_shape().map_err(invalid)?,
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    for to_step in 0..definition.steps().len() {
        let from_step = to_step.saturating_sub(1);
        let this = descriptors
            .get(to_step)
            .ok_or(WorkflowExecutionError::UnknownAction { step: to_step })?;
        let link = definition
            .links()
            .get(to_step)
            .ok_or(WorkflowExecutionError::UnknownAction { step: to_step })?;
        match link {
            LinkKind::Direct => {
                if let Some(previous) = to_step
                    .checked_sub(1)
                    .and_then(|index| descriptors.get(index))
                {
                    if previous.response != this.request {
                        return Err(WorkflowExecutionError::InvalidLink { from_step, to_step });
                    }
                }
            }
            LinkKind::Literal { arguments } => {
                validate_request_shape(&this.request, arguments, &[], from_step, to_step)?;
            }
            LinkKind::Mapping {
                arguments,
                references,
            } => {
                let properties = schema_properties(&this.request)
                    .ok_or(WorkflowExecutionError::InvalidLink { from_step, to_step })?;
                for reference in references {
                    let Some(destination) = properties.get(&reference.dest_field) else {
                        return Err(WorkflowExecutionError::InvalidLink { from_step, to_step });
                    };
                    if reference.selector == SourceSelector::PreviousOutput {
                        let Some(previous) = to_step
                            .checked_sub(1)
                            .and_then(|index| descriptors.get(index))
                        else {
                            return Err(WorkflowExecutionError::PreviousOutputUnavailable {
                                step: to_step,
                            });
                        };
                        match &reference.source_field {
                            Some(field)
                                if schema_properties(&previous.response)
                                    .and_then(|properties| properties.get(field))
                                    == Some(destination) => {}
                            None if &previous.response == destination => {}
                            _ => {
                                return Err(WorkflowExecutionError::InvalidLink {
                                    from_step,
                                    to_step,
                                })
                            }
                        }
                    }
                }
                validate_request_shape(&this.request, arguments, references, from_step, to_step)?;
            }
        }
    }
    Ok(())
}

fn validate_request_shape(
    request_schema: &Value,
    arguments: &Value,
    references: &[FieldRef],
    from_step: usize,
    to_step: usize,
) -> Result<(), WorkflowExecutionError> {
    let properties = schema_properties(request_schema)
        .ok_or(WorkflowExecutionError::InvalidLink { from_step, to_step })?;
    let arguments = arguments
        .as_object()
        .ok_or(WorkflowExecutionError::InvalidLink { from_step, to_step })?;
    if arguments
        .keys()
        .any(|field| !properties.contains_key(field))
        || references
            .iter()
            .any(|reference| !properties.contains_key(&reference.dest_field))
    {
        return Err(WorkflowExecutionError::InvalidLink { from_step, to_step });
    }
    if let Some(required) = request_schema.get("required").and_then(Value::as_array) {
        for required in required {
            let Some(required) = required.as_str() else {
                return Err(WorkflowExecutionError::InvalidLink { from_step, to_step });
            };
            if !arguments.contains_key(required)
                && !references
                    .iter()
                    .any(|reference| reference.dest_field == required)
            {
                return Err(WorkflowExecutionError::InvalidLink { from_step, to_step });
            }
        }
    }
    Ok(())
}

/// Validates every step against currently registered direct Actions.
pub fn validate_definition(
    registry: &WorkflowActionRegistry,
    definition: &WorkflowDefinition,
) -> Result<(), WorkflowControlRejection> {
    let actions = resolve_actions(registry, definition)
        .map_err(|_error| WorkflowControlRejection::UnknownAction)?;
    if definition.has_branch() {
        return Ok(());
    }
    validate_links(definition, &actions).map_err(|_error| WorkflowControlRejection::InvalidLink)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]
    #![allow(missing_docs)]

    use alloc::boxed::Box;
    use alloc::rc::Rc;
    use alloc::string::{String, ToString};
    use alloc::vec::Vec;
    use core::cell::{Cell, RefCell};
    use core::task::Poll;

    use futures_lite::future::{block_on, poll_fn, poll_once};
    use serde_json::{json, Value};

    use super::validate_definition;
    use crate::{
        parse_definition, workflow_action_schema_inline, Event, WorkflowActionFuture,
        WorkflowActionHandler, WorkflowActionRegistry, WorkflowActionSchema,
        WorkflowControlRejection, WorkflowRuntime,
    };

    struct Trigger;

    impl Event for Trigger {
        const ID: &'static str = "test.trigger";
    }

    struct YieldOnceAction<const ACTION: u8> {
        name: &'static str,
        trace: Rc<RefCell<Vec<String>>>,
    }

    impl<const ACTION: u8> WorkflowActionHandler for YieldOnceAction<ACTION> {
        type Request = Value;
        type Response = Value;

        const SCHEMA: WorkflowActionSchema = match ACTION {
            0 => workflow_action_schema_inline!("a.first", "{}", "{}"),
            1 => workflow_action_schema_inline!("b.first", "{}", "{}"),
            _ => workflow_action_schema_inline!("test.invalid", "{}", "{}"),
        };

        fn invoke(&self, input: Value) -> WorkflowActionFuture<'_, Value> {
            let yielded = Cell::new(false);
            let trace = Rc::clone(&self.trace);
            let name = self.name;
            Box::pin(poll_fn(move |context| {
                if !yielded.replace(true) {
                    trace.borrow_mut().push(name.to_string());
                    context.waker().wake_by_ref();
                    Poll::Pending
                } else {
                    Poll::Ready(Ok(input.clone()))
                }
            }))
        }
    }

    struct ReadyAction<const ACTION: u8> {
        name: &'static str,
        trace: Rc<RefCell<Vec<String>>>,
    }

    struct FixedAction<const ACTION: u8> {
        output: Value,
        inputs: Rc<RefCell<Vec<Value>>>,
    }

    impl<const ACTION: u8> WorkflowActionHandler for FixedAction<ACTION> {
        type Request = Value;
        type Response = Value;

        const SCHEMA: WorkflowActionSchema = match ACTION {
            0 => workflow_action_schema_inline!(
                "test.produce",
                "{}",
                r#"{"type":"object","properties":{"token":{"type":"integer"}},"required":["token"]}"#
            ),
            1 => workflow_action_schema_inline!(
                "test.sink",
                r#"{"type":"object","properties":{"token":{"type":"integer"},"extra":{"type":"integer"}},"required":["token","extra"]}"#,
                r#"{"type":"object","properties":{"ok":{"type":"boolean"}}}"#
            ),
            2 => workflow_action_schema_inline!("test.then", "{}", "{}"),
            3 => workflow_action_schema_inline!("test.else", "{}", "{}"),
            _ => workflow_action_schema_inline!("test.invalid", "{}", "{}"),
        };

        fn invoke(&self, input: Value) -> WorkflowActionFuture<'_, Value> {
            self.inputs.borrow_mut().push(input);
            let output = self.output.clone();
            Box::pin(async move { Ok(output) })
        }
    }

    impl<const ACTION: u8> WorkflowActionHandler for ReadyAction<ACTION> {
        type Request = Value;
        type Response = Value;

        const SCHEMA: WorkflowActionSchema = match ACTION {
            0 => workflow_action_schema_inline!("a.second", "{}", "{}"),
            1 => workflow_action_schema_inline!("b.second", "{}", "{}"),
            2 => workflow_action_schema_inline!("test.ready", "{}", "{}"),
            _ => workflow_action_schema_inline!("test.invalid", "{}", "{}"),
        };

        fn invoke(&self, input: Value) -> WorkflowActionFuture<'_, Value> {
            self.trace.borrow_mut().push(self.name.to_string());
            Box::pin(async move { Ok(input) })
        }
    }

    #[test]
    fn executions_advance_concurrently_but_steps_remain_ordered() {
        block_on(async {
            let trace = Rc::new(RefCell::new(Vec::new()));
            let actions = WorkflowActionRegistry::new();
            let _registrations = [
                actions
                    .add_action(YieldOnceAction::<0> {
                        name: "a.first",
                        trace: Rc::clone(&trace),
                    })
                    .expect("register a.first"),
                actions
                    .add_action(ReadyAction::<0> {
                        name: "a.second",
                        trace: Rc::clone(&trace),
                    })
                    .expect("register a.second"),
                actions
                    .add_action(YieldOnceAction::<1> {
                        name: "b.first",
                        trace: Rc::clone(&trace),
                    })
                    .expect("register b.first"),
                actions
                    .add_action(ReadyAction::<1> {
                        name: "b.second",
                        trace: Rc::clone(&trace),
                    })
                    .expect("register b.second"),
            ];
            let mut runtime = WorkflowRuntime::new(actions);
            let control = runtime.control();
            control.load(parse_definition(
                r#"{"id":"a","match":{"event":"test.trigger"},"steps":[{"call":"a.first"},{"call":"a.second"}]}"#,
            ).expect("parse a")).expect("load a");
            control.load(parse_definition(
                r#"{"id":"b","match":{"event":"test.trigger"},"steps":[{"call":"b.first"},{"call":"b.second"}]}"#,
            ).expect("parse b")).expect("load b");

            control.emit::<Trigger>(json!({})).expect("emit");
            assert!(poll_once(&mut runtime).await.is_none());
            assert_eq!(&*trace.borrow(), &["a.first", "b.first"]);

            assert!(poll_once(&mut runtime).await.is_none());
            assert_eq!(
                &*trace.borrow(),
                &["a.first", "b.first", "a.second", "b.second"]
            );
            assert_eq!(runtime.view().info().completed_count, 2);
        });
    }

    #[test]
    fn emitters_see_listeners_and_wait_for_their_backlog() {
        block_on(async {
            let trace = Rc::new(RefCell::new(Vec::new()));
            let actions = WorkflowActionRegistry::new();
            let _registration = actions
                .add_action(YieldOnceAction::<0> {
                    name: "a.first",
                    trace: Rc::clone(&trace),
                })
                .expect("register a.first");
            let mut runtime = WorkflowRuntime::new(actions);
            let control = runtime.control();
            assert!(!control.has_listener::<Trigger>());
            control
                .load(
                    parse_definition(
                        r#"{"id":"a","match":{"event":"test.*"},"steps":[{"call":"a.first"}]}"#,
                    )
                    .expect("parse a"),
                )
                .expect("load a");
            assert!(control.has_listener::<Trigger>());

            let mut ready = Box::pin(control.ready_for::<Trigger>());
            for _event in 0..super::EVENT_BACKLOG_LIMIT {
                assert!(poll_once(&mut ready).await.is_some());
                ready = Box::pin(control.ready_for::<Trigger>());
                control.emit::<Trigger>(json!({})).expect("emit");
            }
            assert!(poll_once(&mut ready).await.is_none(), "backlog is full");

            // Each execution yields once, then completes on the next poll.
            assert!(poll_once(&mut runtime).await.is_none());
            assert!(poll_once(&mut runtime).await.is_none());
            assert_eq!(
                runtime.view().info().completed_count,
                super::EVENT_BACKLOG_LIMIT
            );
            assert!(poll_once(&mut ready).await.is_some(), "backlog drained");
        });
    }

    #[test]
    fn definitions_can_load_and_unload_while_runtime_exists() {
        let actions = WorkflowActionRegistry::new();
        let _registration = actions
            .add_action(ReadyAction::<2> {
                name: "ready",
                trace: Rc::new(RefCell::new(Vec::new())),
            })
            .expect("register Action");
        let runtime = WorkflowRuntime::new(actions);
        let control = runtime.control();
        let definition = parse_definition(
            r#"{"id":"mutable","match":{"event":"test.trigger"},"steps":[{"call":"test.ready"}]}"#,
        )
        .expect("parse definition");
        control.load(definition).expect("load definition");
        assert_eq!(runtime.view().definitions().len(), 1);
        let id = crate::WorkflowId::try_from("mutable").expect("valid id");
        control.unload(&id).expect("unload definition");
        assert!(runtime.view().definitions().is_empty());
    }

    #[test]
    fn dropping_registration_removes_action() {
        let registry = WorkflowActionRegistry::new();
        let registration = registry
            .add_action(ReadyAction::<2> {
                name: "ready",
                trace: Rc::new(RefCell::new(Vec::new())),
            })
            .expect("register Action");
        assert_eq!(registry.descriptors().len(), 1);
        drop(registration);
        assert!(registry.descriptors().is_empty());
    }

    struct CaptureText {
        requests: Rc<RefCell<Vec<crate::JsonText>>>,
    }

    impl WorkflowActionHandler for CaptureText {
        type Request = crate::JsonText;
        type Response = Value;

        const SCHEMA: WorkflowActionSchema = workflow_action_schema_inline!(
            "test.capture",
            r#"{"type":"object","properties":{"payload":{"type":"object"},"kind":{"type":"string"},"x":{"type":"string"}}}"#,
            "{}"
        );

        fn invoke(&self, request: crate::JsonText) -> WorkflowActionFuture<'_, Value> {
            self.requests.borrow_mut().push(request);
            Box::pin(async { Ok(json!({})) })
        }
    }

    #[test]
    fn mapping_splices_referenced_fields_as_stored_text() {
        block_on(async {
            let actions = WorkflowActionRegistry::new();
            let requests = Rc::new(RefCell::new(Vec::new()));
            let _registration = actions
                .add_action(CaptureText {
                    requests: Rc::clone(&requests),
                })
                .expect("register capture");
            let mut runtime = WorkflowRuntime::new(actions);
            let control = runtime.control();
            control.load(parse_definition(
                r#"{"id":"splice","match":{"event":"test.trigger"},"steps":[{"call":"test.capture","arguments":{"payload":"$event.input.payload","kind":"x"}}]}"#,
            ).expect("parse splice")).expect("load splice");
            control.load(parse_definition(
                r#"{"id":"missing","match":{"event":"test.trigger"},"steps":[{"call":"test.capture","arguments":{"x":"$event.input.missing"}}]}"#,
            ).expect("parse missing")).expect("load missing");

            control
                .emit::<Trigger>(json!({ "payload": { "text": "a\"b", "n": [1, 2] }, "other": 1 }))
                .expect("emit");
            assert!(poll_once(&mut runtime).await.is_none());

            let requests = requests.borrow();
            assert_eq!(requests.len(), 1);
            assert_eq!(
                requests.first().expect("request").as_str(),
                r#"{"kind":"x","payload":{"n":[1,2],"text":"a\"b"}}"#
            );
            let info = runtime.view().info();
            assert_eq!((info.completed_count, info.failed_count), (1, 1));
            assert_eq!(
                info.last_failure.expect("failure").error(),
                &super::WorkflowExecutionError::MissingEventInputField {
                    field: "missing".into()
                }
            );
        });
    }

    #[test]
    fn mapping_and_branching_use_direct_json_values() {
        block_on(async {
            let actions = WorkflowActionRegistry::new();
            let produced_inputs = Rc::new(RefCell::new(Vec::new()));
            let sink_inputs = Rc::new(RefCell::new(Vec::new()));
            let then_inputs = Rc::new(RefCell::new(Vec::new()));
            let else_inputs = Rc::new(RefCell::new(Vec::new()));
            let registrations = [
                actions
                    .add_action(FixedAction::<0> {
                        output: json!({ "token": 7 }),
                        inputs: Rc::clone(&produced_inputs),
                    })
                    .expect("register producer"),
                actions
                    .add_action(FixedAction::<1> {
                        output: json!({ "ok": true }),
                        inputs: Rc::clone(&sink_inputs),
                    })
                    .expect("register sink"),
                actions
                    .add_action(FixedAction::<2> {
                        output: json!({}),
                        inputs: Rc::clone(&then_inputs),
                    })
                    .expect("register then"),
                actions
                    .add_action(FixedAction::<3> {
                        output: json!({}),
                        inputs: Rc::clone(&else_inputs),
                    })
                    .expect("register else"),
            ];
            let mut runtime = WorkflowRuntime::new(actions);
            runtime.control().load(parse_definition(
                r#"{"id":"mapped","match":{"event":"test.trigger"},"steps":[{"call":"test.produce"},{"call":"test.sink","arguments":{"token":"$previous.output.token","extra":5}},{"if":{"source":"$previous.output.ok","equals":true},"then":[{"call":"test.then"}],"else":[{"call":"test.else"}]}]}"#,
            ).expect("parse mapped Workflow")).expect("load mapped Workflow");
            runtime
                .control()
                .emit::<Trigger>(json!({ "source": "event" }))
                .expect("emit");
            assert!(poll_once(&mut runtime).await.is_none());

            assert_eq!(&*produced_inputs.borrow(), &[json!({ "source": "event" })]);
            assert_eq!(&*sink_inputs.borrow(), &[json!({ "token": 7, "extra": 5 })]);
            assert_eq!(&*then_inputs.borrow(), &[json!({ "ok": true })]);
            assert!(else_inputs.borrow().is_empty());
            assert_eq!(runtime.view().info().completed_count, 1);
            drop(registrations);
        });
    }

    #[test]
    fn link_validation_checks_schemas_of_adjacent_steps() {
        let actions = WorkflowActionRegistry::new();
        let inputs = Rc::new(RefCell::new(Vec::new()));
        let _registrations = [
            actions
                .add_action(FixedAction::<0> {
                    output: json!({ "token": 7 }),
                    inputs: Rc::clone(&inputs),
                })
                .expect("register producer"),
            actions
                .add_action(FixedAction::<1> {
                    output: json!({ "ok": true }),
                    inputs: Rc::clone(&inputs),
                })
                .expect("register sink"),
        ];
        let validate = |sink: &str| {
            let source = alloc::format!(
                r#"{{"id":"linked","match":{{"event":"test.trigger"}},"steps":[{{"call":"test.produce"}},{sink}]}}"#
            );
            validate_definition(&actions, &parse_definition(&source).expect("parse"))
        };

        assert_eq!(
            validate(
                r#"{"call":"test.sink","arguments":{"token":"$previous.output.token","extra":5}}"#
            ),
            Ok(())
        );
        for invalid in [
            r#"{"call":"test.sink"}"#,
            r#"{"call":"test.sink","arguments":{"token":"$previous.output.token"}}"#,
            r#"{"call":"test.sink","arguments":{"token":1,"extra":2,"bogus":3}}"#,
            r#"{"call":"test.sink","arguments":{"token":"$previous.output.missing","extra":5}}"#,
        ] {
            assert_eq!(
                validate(invalid),
                Err(WorkflowControlRejection::InvalidLink),
                "{invalid}"
            );
        }
    }
}
