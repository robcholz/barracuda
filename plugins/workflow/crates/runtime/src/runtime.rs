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

use crate::action::WorkflowAction;
use crate::definition::{WorkflowCondition, WorkflowOperation};
use crate::link::{FieldRef, LinkKind, SourceSelector};
use crate::{
    EmitError, Event, EventId, Topic, WorkflowActionDescriptor, WorkflowActionRegistry,
    WorkflowControlRejection, WorkflowDefinition, WorkflowId, WorkflowLoadError,
    WorkflowUnloadError, WorkflowValue,
};

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

    fn dispatch(&self, event_id: &EventId, topic: Option<&Topic>, input: WorkflowValue) {
        let plans = self.matching_plans(event_id, topic);
        log::debug!(
            "Workflow Event `{}` matched {} definition(s)",
            event_id.as_str(),
            plans.len()
        );
        let input = Rc::new(input);
        let mut executions = VecDeque::new();
        for plan in plans {
            match WorkflowExecution::new(plan, Rc::clone(&input), &self.actions) {
                Ok(execution) => executions.push_back(execution),
                Err((workflow_id, error)) => self.record_failure(workflow_id, error),
            }
        }
        if executions.is_empty() {
            return;
        }
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

    /// Emits one typed Event directly into the Workflow Runtime.
    pub fn emit<E>(&self, input: WorkflowValue) -> Result<(), EmitError>
    where
        E: Event,
    {
        self.emit_inner::<E>(None, input)
    }

    /// Emits one typed Event with a topic filter.
    pub fn emit_to<E>(&self, topic: Topic, input: WorkflowValue) -> Result<(), EmitError>
    where
        E: Event,
    {
        self.emit_inner::<E>(Some(topic), input)
    }

    fn emit_inner<E>(&self, topic: Option<Topic>, input: WorkflowValue) -> Result<(), EmitError>
    where
        E: Event,
    {
        let event_id = EventId::try_from(E::ID)?;
        self.shared.dispatch(&event_id, topic.as_ref(), input);
        Ok(())
    }

    /// Emits a dynamically identified Event.
    pub fn emit_event(&self, event_id: EventId, topic: Option<Topic>, input: WorkflowValue) {
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
                }
                Poll::Ready(Err(error)) => {
                    log::error!("Workflow `{}` failed: {error}", execution.id.as_str());
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
    driver: WorkflowDriver,
}

impl WorkflowExecution {
    fn new(
        plan: WorkflowPlan,
        event: Rc<WorkflowValue>,
        registry: &WorkflowActionRegistry,
    ) -> Result<Self, (WorkflowId, WorkflowExecutionError)> {
        let id = plan.definition.id().clone();
        let actions = match resolve_actions(registry, &plan.definition) {
            Ok(actions) => actions,
            Err(error) => return Err((id, error)),
        };
        if !plan.definition.has_branch() {
            let descriptors = actions
                .iter()
                .map(|action| action.descriptor().clone())
                .collect::<Vec<_>>();
            if let Err(error) = validate_links(&plan.definition, &descriptors) {
                return Err((id, error));
            }
        }
        let definition = Rc::clone(&plan.definition);
        let driver = Box::pin(async move { execute_steps(&definition, &event, &actions).await });
        Ok(Self { id, driver })
    }
}

async fn execute_steps(
    definition: &WorkflowDefinition,
    event_input: &WorkflowValue,
    actions: &[Rc<dyn WorkflowAction>],
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
    event_input: &WorkflowValue,
    actions: &[Rc<dyn WorkflowAction>],
    previous: Option<WorkflowValue>,
    _previous_step: Option<usize>,
) -> Result<WorkflowValue, WorkflowExecutionError> {
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
        LinkKind::Literal { arguments } => arguments.clone(),
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
        .invoke(request)
        .await
        .map_err(|source| WorkflowExecutionError::Action {
            step: to_step,
            source,
        })
}

fn evaluate_condition(
    condition: &WorkflowCondition,
    event_input: &WorkflowValue,
    previous: Option<&WorkflowValue>,
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
    let missing = Value::Null;
    let actual = match &condition.field {
        None => source,
        Some(field) => source
            .as_object()
            .ok_or(match condition.selector {
                SourceSelector::EventInput => WorkflowExecutionError::EventInputNotObject,
                SourceSelector::PreviousOutput => WorkflowExecutionError::ResponseNotObject {
                    step: previous_step.unwrap_or(0),
                },
            })?
            .get(field)
            .unwrap_or(&missing),
    };
    Ok(condition.comparison.matches(actual))
}

fn mapped_request(
    event_input: &WorkflowValue,
    previous: Option<&WorkflowValue>,
    to_step: usize,
    arguments: &WorkflowValue,
    references: &[FieldRef],
) -> Result<WorkflowValue, WorkflowExecutionError> {
    let mut request =
        arguments
            .as_object()
            .cloned()
            .ok_or(WorkflowExecutionError::InvalidLink {
                from_step: to_step.saturating_sub(1),
                to_step,
            })?;
    for reference in references {
        let source = match reference.selector {
            SourceSelector::EventInput => event_input,
            SourceSelector::PreviousOutput => previous
                .ok_or(WorkflowExecutionError::PreviousOutputUnavailable { step: to_step })?,
        };
        let value = match &reference.source_field {
            None => source.clone(),
            Some(field) => {
                let object = source.as_object().ok_or(match reference.selector {
                    SourceSelector::EventInput => WorkflowExecutionError::EventInputNotObject,
                    SourceSelector::PreviousOutput => WorkflowExecutionError::ResponseNotObject {
                        step: to_step.saturating_sub(1),
                    },
                })?;
                object
                    .get(field)
                    .cloned()
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
        request.insert(reference.dest_field.clone(), value);
    }
    Ok(Value::Object(request))
}

fn resolve_actions(
    registry: &WorkflowActionRegistry,
    definition: &WorkflowDefinition,
) -> Result<Vec<Rc<dyn WorkflowAction>>, WorkflowExecutionError> {
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

fn validate_links(
    definition: &WorkflowDefinition,
    descriptors: &[WorkflowActionDescriptor],
) -> Result<(), WorkflowExecutionError> {
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
                    if previous.response_schema() != this.request_schema() {
                        return Err(WorkflowExecutionError::InvalidLink { from_step, to_step });
                    }
                }
            }
            LinkKind::Literal { arguments } => {
                validate_request_shape(this.request_schema(), arguments, &[], from_step, to_step)?;
            }
            LinkKind::Mapping {
                arguments,
                references,
            } => {
                let properties = schema_properties(this.request_schema())
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
                                if schema_properties(previous.response_schema())
                                    .and_then(|properties| properties.get(field))
                                    == Some(destination) => {}
                            None if previous.response_schema() == destination => {}
                            _ => {
                                return Err(WorkflowExecutionError::InvalidLink {
                                    from_step,
                                    to_step,
                                })
                            }
                        }
                    }
                }
                validate_request_shape(
                    this.request_schema(),
                    arguments,
                    references,
                    from_step,
                    to_step,
                )?;
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
    let descriptors = actions
        .iter()
        .map(|action| action.descriptor().clone())
        .collect::<Vec<_>>();
    validate_links(definition, &descriptors).map_err(|_error| WorkflowControlRejection::InvalidLink)
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

    use crate::{
        parse_definition, Event, WorkflowAction, WorkflowActionAddress, WorkflowActionDescriptor,
        WorkflowActionError, WorkflowActionFuture, WorkflowActionRegistry, WorkflowRuntime,
    };

    struct Trigger;

    impl Event for Trigger {
        const ID: &'static str = "test.trigger";
    }

    struct YieldOnceAction {
        descriptor: WorkflowActionDescriptor,
        name: &'static str,
        trace: Rc<RefCell<Vec<String>>>,
    }

    impl WorkflowAction for YieldOnceAction {
        fn descriptor(&self) -> &WorkflowActionDescriptor {
            &self.descriptor
        }

        fn invoke(&self, input: Value) -> WorkflowActionFuture<'_> {
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

    struct ReadyAction {
        descriptor: WorkflowActionDescriptor,
        name: &'static str,
        trace: Rc<RefCell<Vec<String>>>,
    }

    struct FixedAction {
        descriptor: WorkflowActionDescriptor,
        output: Value,
        inputs: Rc<RefCell<Vec<Value>>>,
    }

    impl WorkflowAction for FixedAction {
        fn descriptor(&self) -> &WorkflowActionDescriptor {
            &self.descriptor
        }

        fn invoke(&self, input: Value) -> WorkflowActionFuture<'_> {
            self.inputs.borrow_mut().push(input);
            let output = self.output.clone();
            Box::pin(async move { Ok(output) })
        }
    }

    impl WorkflowAction for ReadyAction {
        fn descriptor(&self) -> &WorkflowActionDescriptor {
            &self.descriptor
        }

        fn invoke(&self, input: Value) -> WorkflowActionFuture<'_> {
            self.trace.borrow_mut().push(self.name.to_string());
            Box::pin(async move { Ok(input) })
        }
    }

    fn descriptor(address: &str) -> WorkflowActionDescriptor {
        WorkflowActionDescriptor::new(
            WorkflowActionAddress::try_from(address).expect("valid Action address"),
            address,
            json!({}),
            json!({}),
        )
    }

    #[test]
    fn executions_advance_concurrently_but_steps_remain_ordered() {
        block_on(async {
            let trace = Rc::new(RefCell::new(Vec::new()));
            let actions = WorkflowActionRegistry::new();
            let _registrations = [
                actions
                    .register(YieldOnceAction {
                        descriptor: descriptor("a.first"),
                        name: "a.first",
                        trace: Rc::clone(&trace),
                    })
                    .expect("register a.first"),
                actions
                    .register(ReadyAction {
                        descriptor: descriptor("a.second"),
                        name: "a.second",
                        trace: Rc::clone(&trace),
                    })
                    .expect("register a.second"),
                actions
                    .register(YieldOnceAction {
                        descriptor: descriptor("b.first"),
                        name: "b.first",
                        trace: Rc::clone(&trace),
                    })
                    .expect("register b.first"),
                actions
                    .register(ReadyAction {
                        descriptor: descriptor("b.second"),
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
    fn definitions_can_load_and_unload_while_runtime_exists() {
        let actions = WorkflowActionRegistry::new();
        let _registration = actions
            .register(ReadyAction {
                descriptor: descriptor("test.ready"),
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
            .register(ReadyAction {
                descriptor: descriptor("test.ready"),
                name: "ready",
                trace: Rc::new(RefCell::new(Vec::new())),
            })
            .expect("register Action");
        assert_eq!(registry.descriptors().len(), 1);
        drop(registration);
        assert!(registry.descriptors().is_empty());
    }

    #[test]
    fn mapping_and_branching_use_direct_json_values() {
        block_on(async {
            let actions = WorkflowActionRegistry::new();
            let produced_inputs = Rc::new(RefCell::new(Vec::new()));
            let sink_inputs = Rc::new(RefCell::new(Vec::new()));
            let then_inputs = Rc::new(RefCell::new(Vec::new()));
            let else_inputs = Rc::new(RefCell::new(Vec::new()));
            let token_schema = json!({
                "type": "object",
                "properties": { "token": { "type": "integer" } },
                "required": ["token"]
            });
            let delivery_schema = json!({
                "type": "object",
                "properties": {
                    "token": { "type": "integer" },
                    "extra": { "type": "integer" }
                },
                "required": ["token", "extra"]
            });
            let registrations = [
                actions.register(FixedAction {
                    descriptor: WorkflowActionDescriptor::new(
                        WorkflowActionAddress::try_from("test.produce").expect("address"),
                        "produce",
                        json!({}),
                        token_schema,
                    ),
                    output: json!({ "token": 7 }),
                    inputs: Rc::clone(&produced_inputs),
                }).expect("register producer"),
                actions.register(FixedAction {
                    descriptor: WorkflowActionDescriptor::new(
                        WorkflowActionAddress::try_from("test.sink").expect("address"),
                        "sink",
                        delivery_schema,
                        json!({ "type": "object", "properties": { "ok": { "type": "boolean" } } }),
                    ),
                    output: json!({ "ok": true }),
                    inputs: Rc::clone(&sink_inputs),
                }).expect("register sink"),
                actions.register(FixedAction {
                    descriptor: descriptor("test.then"),
                    output: json!({}),
                    inputs: Rc::clone(&then_inputs),
                }).expect("register then"),
                actions.register(FixedAction {
                    descriptor: descriptor("test.else"),
                    output: json!({}),
                    inputs: Rc::clone(&else_inputs),
                }).expect("register else"),
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

    #[allow(dead_code)]
    fn action_error_is_public() -> WorkflowActionError {
        WorkflowActionError::new("failed")
    }
}
