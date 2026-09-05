//! Cooperative owner and driver for loaded Workflow definitions.

use alloc::borrow::Cow;
use alloc::boxed::Box;
use alloc::collections::VecDeque;
use alloc::rc::Rc;
use alloc::vec::Vec;
use core::cell::{Cell, RefCell};
use core::future::Future;
use core::pin::Pin;
use core::task::{Context, Poll, Waker};

use super::ingress::{EventInput, EventInputPool};
use super::link::{FieldRef, LinkKind, SourceSelector};
use barracuda_rpc::{
    JsonHandler, JsonObjectFields, JsonObjectPayload, JsonObjectWriter, JsonPayload, JsonRef,
    JsonRpcInfo, JsonWriter, RpcClient, RpcContext, RpcError, RpcResult,
};
use getset::Getters;
use serde::de::{Error as _, MapAccess, Visitor};
use serde::Deserializer as _;
use serde_json::value::RawValue;
use serde_json::{Map, Value};

use super::{
    EventId, Topic, WorkflowControlRejection, WorkflowDefinition, WorkflowId, WorkflowLoadError,
    WorkflowUnloadError,
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
    /// Adjacent JSON schemas do not support the requested link.
    #[error("Workflow JSON link from step {from_step} to step {to_step} is incompatible")]
    InvalidLink {
        /// Step producing the JSON response.
        from_step: usize,
        /// Step receiving the JSON request.
        to_step: usize,
    },
    /// A registered JSON RPC exposed malformed JSON Schema text.
    #[error("Workflow JSON RPC step {step} has an invalid schema")]
    InvalidSchema {
        /// Step whose registered schema is invalid.
        step: usize,
    },
    /// A Mapping link requires the previous response to be a JSON object.
    #[error("Workflow JSON RPC step {step} response is not an object")]
    ResponseNotObject {
        /// Step that produced the non-object response.
        step: usize,
    },
    /// A Mapping link referenced a field absent from the actual response.
    #[error("Workflow JSON RPC step {step} response omitted field {field}")]
    MissingOutputField {
        /// Step that produced the response.
        step: usize,
        /// Missing top-level response field.
        field: alloc::string::String,
    },
    /// A Mapping link requires the Event input to be a JSON object.
    #[error("Workflow Event input is not an object")]
    EventInputNotObject,
    /// A Mapping link referenced a field absent from the actual Event input.
    #[error("Workflow Event input omitted field {field}")]
    MissingEventInputField {
        /// Missing top-level Event input field.
        field: alloc::string::String,
    },
    /// The first step attempted to select a previous step that does not exist.
    #[error("Workflow JSON RPC step {step} has no previous output")]
    PreviousOutputUnavailable {
        /// Step whose arguments contain the invalid selector.
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
    definition: Rc<WorkflowDefinition>,
}

struct SharedState {
    definitions: Vec<Rc<WorkflowDefinition>>,
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
        Ok(match Rc::try_unwrap(definition) {
            Ok(definition) => definition,
            Err(definition) => definition.as_ref().clone(),
        })
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
    pub fn ingress_handler<const N: usize, const M: usize>(&self) -> impl JsonHandler + 'static {
        let shared = Rc::clone(&self.shared);
        let event_inputs = Rc::new(EventInputPool::new(N, M));
        move |rpc_context: RpcContext, request: JsonRef, response: JsonWriter| {
            let shared = Rc::clone(&shared);
            let event_inputs = Rc::clone(&event_inputs);
            async move { handle_emit(shared, event_inputs, rpc_context, request, response).await }
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
    driver: WorkflowDriver,
}

impl WorkflowExecution {
    fn new(
        plan: WorkflowPlan,
        event: Rc<EventInput>,
        client: &RpcClient,
        cancellation: Rc<Cell<bool>>,
    ) -> RpcResult<Self> {
        let mut infos = Vec::with_capacity(plan.definition.steps().len());
        for step in plan.definition.steps() {
            infos.push(client.json_method_info(step.address())?);
        }
        let validation = validate_links(&plan.definition, &infos);
        let execution_client = client.clone();
        let definition = Rc::clone(&plan.definition);
        let driver = Box::pin(async move {
            validation?;
            execute_steps(&definition, &event, execution_client).await
        });
        Ok(Self {
            id: plan.definition.id().clone(),
            cancellation,
            driver,
        })
    }

    fn poll(&mut self, context: &mut Context<'_>) -> Poll<Result<(), WorkflowExecutionError>> {
        if self.cancellation.get() {
            return Poll::Ready(Err(WorkflowExecutionError::IngressCancelled));
        }
        self.driver.as_mut().poll(context)
    }
}

fn step_error(step: usize, source: RpcError) -> WorkflowExecutionError {
    WorkflowExecutionError::Rpc { step, source }
}

async fn execute_steps(
    definition: &WorkflowDefinition,
    event_input: &EventInput,
    client: RpcClient,
) -> Result<(), WorkflowExecutionError> {
    let mut previous: Option<JsonRef> = None;
    for to_step in 0..definition.steps().len() {
        let from_step = to_step.saturating_sub(1);
        let step = definition
            .steps()
            .get(to_step)
            .ok_or_else(|| step_error(to_step, RpcError::InvalidFrameState))?;
        let link = definition
            .link(to_step)
            .ok_or_else(|| internal_error(to_step))?;
        let response = match link {
            LinkKind::Direct => {
                let call = match &previous {
                    Some(previous) => {
                        let request = previous
                            .as_str()
                            .map_err(|source| step_error(from_step, source))?;
                        client.call_json(step.address(), request)
                    }
                    None => client.call_json(step.address(), event_input),
                };
                let response = call
                    .map_err(|source| step_error(to_step, source))?
                    .await
                    .map_err(|source| step_error(to_step, source))?;
                response
            }
            LinkKind::Literal { arguments } => {
                let response = client
                    .call_json(step.address(), arguments)
                    .map_err(|source| step_error(to_step, source))?
                    .await
                    .map_err(|source| step_error(to_step, source))?;
                response
            }
            LinkKind::Mapping {
                arguments,
                references,
            } => {
                let previous_output = previous
                    .as_ref()
                    .map(JsonRef::as_str)
                    .transpose()
                    .map_err(|source| step_error(from_step, source))?;
                let request =
                    mapped_request(event_input, previous_output, to_step, arguments, references)?;
                let response = client
                    .call_json(step.address(), &request)
                    .map_err(|source| step_error(to_step, source))?
                    .await
                    .map_err(|source| step_error(to_step, source))?;
                response
            }
        };
        previous = Some(response);
    }
    Ok(())
}

fn mapped_request<'source, 'definition>(
    event_input: &'source EventInput,
    previous_output: Option<&'source str>,
    to_step: usize,
    arguments: &'definition Value,
    references: &'definition [FieldRef],
) -> Result<MappedJson<'source, 'definition>, WorkflowExecutionError> {
    {
        let source = event_input
            .as_str()
            .map_err(|source| step_error(to_step, source))?;
        validate_mapping_source(&source, references, SourceSelector::EventInput, to_step)?;
    }
    if references
        .iter()
        .any(|reference| reference.selector == SourceSelector::PreviousOutput)
    {
        let previous_output = previous_output
            .ok_or(WorkflowExecutionError::PreviousOutputUnavailable { step: to_step })?;
        validate_mapping_source(
            previous_output,
            references,
            SourceSelector::PreviousOutput,
            to_step,
        )?;
    }
    let arguments = match arguments {
        Value::Object(arguments) => arguments,
        _ => {
            return Err(WorkflowExecutionError::InvalidLink {
                from_step: to_step.saturating_sub(1),
                to_step,
            });
        }
    };
    Ok(MappedJson {
        event_input,
        previous_output,
        arguments,
        references,
    })
}

fn validate_mapping_source(
    source: &str,
    references: &[FieldRef],
    selector: SourceSelector,
    to_step: usize,
) -> Result<(), WorkflowExecutionError> {
    if !references
        .iter()
        .any(|reference| reference.selector == selector)
    {
        return Ok(());
    }
    if source
        .as_bytes()
        .iter()
        .find(|byte| !byte.is_ascii_whitespace())
        != Some(&b'{')
    {
        return Err(match selector {
            SourceSelector::EventInput => WorkflowExecutionError::EventInputNotObject,
            SourceSelector::PreviousOutput => WorkflowExecutionError::ResponseNotObject {
                step: to_step.saturating_sub(1),
            },
        });
    }
    let expected = references
        .iter()
        .filter(|reference| reference.selector == selector)
        .count();
    let matched = count_mapping_values(source, references, selector)
        .map_err(|source| step_error(to_step.saturating_sub(1), source))?;
    if matched == expected {
        return Ok(());
    }
    for reference in references
        .iter()
        .filter(|reference| reference.selector == selector)
    {
        let matched = count_mapping_values(source, core::slice::from_ref(reference), selector)
            .map_err(|source| step_error(to_step.saturating_sub(1), source))?;
        if matched == 0 {
            return Err(match selector {
                SourceSelector::EventInput => WorkflowExecutionError::MissingEventInputField {
                    field: reference.source_field.clone(),
                },
                SourceSelector::PreviousOutput => WorkflowExecutionError::MissingOutputField {
                    step: to_step.saturating_sub(1),
                    field: reference.source_field.clone(),
                },
            });
        }
    }
    Err(step_error(to_step.saturating_sub(1), RpcError::InvalidJson))
}

struct MappedJson<'source, 'definition> {
    event_input: &'source EventInput,
    previous_output: Option<&'source str>,
    arguments: &'definition Map<alloc::string::String, Value>,
    references: &'definition [FieldRef],
}

impl JsonPayload for MappedJson<'_, '_> {
    fn encoded_len(&self) -> RpcResult<usize> {
        JsonObjectPayload::new(self).encoded_len()
    }

    fn write_json(&self, destination: &mut [u8]) -> RpcResult<usize> {
        JsonObjectPayload::new(self).write_json(destination)
    }
}

impl JsonObjectFields for MappedJson<'_, '_> {
    fn write_fields(&self, writer: &mut JsonObjectWriter<'_>) -> RpcResult<()> {
        for (name, value) in self.arguments {
            writer.field(name, value)?;
        }
        let event_input = self.event_input.as_str()?;
        write_mapping_source(
            writer,
            &event_input,
            self.references,
            SourceSelector::EventInput,
        )?;
        if let Some(previous_output) = self.previous_output {
            write_mapping_source(
                writer,
                previous_output,
                self.references,
                SourceSelector::PreviousOutput,
            )?;
        }
        Ok(())
    }
}

fn write_mapping_source(
    writer: &mut JsonObjectWriter<'_>,
    source: &str,
    references: &[FieldRef],
    selector: SourceSelector,
) -> RpcResult<()> {
    let expected = references
        .iter()
        .filter(|reference| reference.selector == selector)
        .count();
    if expected == 0 {
        return Ok(());
    }
    let mut deserializer = serde_json::Deserializer::from_str(source);
    let matched = deserializer
        .deserialize_map(WriteMappingFields {
            writer,
            references,
            selector,
        })
        .map_err(|_error| RpcError::InvalidJson)?;
    deserializer.end().map_err(|_error| RpcError::InvalidJson)?;
    if matched != expected {
        return Err(RpcError::InvalidJson);
    }
    Ok(())
}

fn count_mapping_values(
    source: &str,
    references: &[FieldRef],
    selector: SourceSelector,
) -> RpcResult<usize> {
    let mut deserializer = serde_json::Deserializer::from_str(source);
    let matched = deserializer
        .deserialize_map(CountMappingFields {
            references,
            selector,
        })
        .map_err(|_error| RpcError::InvalidJson)?;
    deserializer.end().map_err(|_error| RpcError::InvalidJson)?;
    Ok(matched)
}

struct CountMappingFields<'a> {
    references: &'a [FieldRef],
    selector: SourceSelector,
}

impl<'de> Visitor<'de> for CountMappingFields<'_> {
    type Value = usize;

    fn expecting(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter.write_str("a JSON object")
    }

    fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let mut matched = 0usize;
        while let Some(name) = map.next_key::<Cow<'de, str>>()? {
            let _value = map.next_value::<&RawValue>()?;
            let field_matches = self
                .references
                .iter()
                .filter(|reference| {
                    reference.selector == self.selector && reference.source_field == name.as_ref()
                })
                .count();
            matched = matched
                .checked_add(field_matches)
                .ok_or_else(|| A::Error::custom("too many Workflow mapping fields"))?;
        }
        Ok(matched)
    }
}

struct WriteMappingFields<'writer, 'output, 'definition> {
    writer: &'writer mut JsonObjectWriter<'output>,
    references: &'definition [FieldRef],
    selector: SourceSelector,
}

impl<'de> Visitor<'de> for WriteMappingFields<'_, '_, '_> {
    type Value = usize;

    fn expecting(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter.write_str("a JSON object")
    }

    fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let mut matched = 0usize;
        while let Some(name) = map.next_key::<Cow<'de, str>>()? {
            let value = map.next_value::<&RawValue>()?;
            for reference in self.references.iter().filter(|reference| {
                reference.selector == self.selector && reference.source_field == name.as_ref()
            }) {
                self.writer
                    .field(&reference.dest_field, value.get())
                    .map_err(A::Error::custom)?;
                matched = matched
                    .checked_add(1)
                    .ok_or_else(|| A::Error::custom("too many Workflow mapping fields"))?;
            }
        }
        Ok(matched)
    }
}

/// Validates each JSON request assembled from Event input, literals, or the
/// previous JSON response.
fn validate_links(
    definition: &WorkflowDefinition,
    infos: &[JsonRpcInfo],
) -> Result<(), WorkflowExecutionError> {
    for to_step in 0..definition.steps().len() {
        let from_step = to_step.saturating_sub(1);
        let this = infos.get(to_step).ok_or(internal_error(to_step))?;
        let kind = definition.link(to_step).ok_or(internal_error(to_step))?;
        let request_schema = parse_schema(this.request_schema().as_str(), to_step)?;
        match kind {
            LinkKind::Direct => {
                if let Some(prev) = to_step.checked_sub(1).and_then(|index| infos.get(index)) {
                    let response_schema = parse_schema(prev.response_schema().as_str(), from_step)?;
                    if response_schema != request_schema {
                        return Err(WorkflowExecutionError::InvalidLink { from_step, to_step });
                    }
                }
            }
            LinkKind::Literal { arguments } => {
                validate_request_shape(&request_schema, arguments, &[], from_step, to_step)?;
                validate_request_size(this, arguments, to_step)?;
            }
            LinkKind::Mapping {
                arguments,
                references,
            } => {
                let request_properties = schema_properties(&request_schema)
                    .ok_or(WorkflowExecutionError::InvalidLink { from_step, to_step })?;
                for reference in references {
                    let destination = request_properties.get(&reference.dest_field);
                    let Some(destination) = destination else {
                        return Err(WorkflowExecutionError::InvalidLink { from_step, to_step });
                    };
                    if reference.selector == SourceSelector::PreviousOutput {
                        let Some(prev) = to_step.checked_sub(1).and_then(|index| infos.get(index))
                        else {
                            return Err(WorkflowExecutionError::PreviousOutputUnavailable {
                                step: to_step,
                            });
                        };
                        let response_schema =
                            parse_schema(prev.response_schema().as_str(), from_step)?;
                        let response_properties = schema_properties(&response_schema)
                            .ok_or(WorkflowExecutionError::InvalidLink { from_step, to_step })?;
                        if response_properties.get(&reference.source_field) != Some(destination) {
                            return Err(WorkflowExecutionError::InvalidLink { from_step, to_step });
                        }
                    }
                }
                validate_request_shape(&request_schema, arguments, references, from_step, to_step)?;
                validate_request_size(this, arguments, to_step)?;
            }
        }
    }
    Ok(())
}

fn parse_schema(schema: &str, step: usize) -> Result<Value, WorkflowExecutionError> {
    serde_json::from_str(schema).map_err(|_error| WorkflowExecutionError::InvalidSchema { step })
}

fn schema_properties(schema: &Value) -> Option<&Map<alloc::string::String, Value>> {
    schema.get("properties")?.as_object()
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
                return Err(WorkflowExecutionError::InvalidSchema { step: to_step });
            };
            let supplied = arguments.contains_key(required)
                || references
                    .iter()
                    .any(|reference| reference.dest_field == required);
            if !supplied {
                return Err(WorkflowExecutionError::InvalidLink { from_step, to_step });
            }
        }
    }
    Ok(())
}

fn validate_request_size(
    info: &JsonRpcInfo,
    arguments: &Value,
    step: usize,
) -> Result<(), WorkflowExecutionError> {
    let size = arguments
        .encoded_len()
        .map_err(|source| step_error(step, source))?;
    if size > info.max_request_bytes() {
        return Err(step_error(
            step,
            RpcError::FrameTooLarge {
                size,
                capacity: info.max_request_bytes(),
            },
        ));
    }
    Ok(())
}

/// Validates every step link of `definition` against the methods currently
/// registered with `client`.
///
/// This is the primary gate for `workflow.load`: every address must resolve to
/// a JSON RPC, adjacent Direct and `$previous.output.*` links must have
/// compatible schemas, and every Literal/Mapping request must supply the
/// target's required fields. Event fields are resolved from the actual matched
/// Event document at execution time because one rule may match multiple Event
/// IDs.
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
            .json_method_info(step.address())
            .map_err(|_error| WorkflowControlRejection::UnknownMethod)?;
        infos.push(info);
    }
    validate_links(definition, &infos).map_err(|_error| WorkflowControlRejection::InvalidLink)
}

fn internal_error(step: usize) -> WorkflowExecutionError {
    WorkflowExecutionError::Rpc {
        step,
        source: RpcError::InvalidFrameState,
    }
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

async fn handle_emit(
    shared: Rc<RuntimeShared>,
    event_inputs: Rc<EventInputPool>,
    _context: RpcContext,
    request: JsonRef,
    response: JsonWriter,
) -> RpcResult<()> {
    let (header, event) = EventInput::accept(request, &event_inputs)?;
    let plans = shared.matching_plans(header.event_id(), header.topic());
    if plans.is_empty() {
        return response.write("{}").await;
    }
    let Some(execution_client) = shared.execution_client() else {
        return Err(RpcError::InvalidFrameState);
    };

    let event = Rc::new(event);
    let cancellation = Rc::new(Cell::new(false));
    let mut executions = Vec::with_capacity(plans.len());
    for plan in plans {
        executions.push(WorkflowExecution::new(
            plan,
            Rc::clone(&event),
            &execution_client,
            Rc::clone(&cancellation),
        )?);
    }
    let guard = DispatchGuard::new(Rc::clone(&shared), cancellation);
    shared.enqueue(executions);
    response.write("{}").await?;
    guard.accept();
    Ok(())
}

#[cfg(test)]
mod json_workflow_tests {
    #![allow(clippy::expect_used)]
    #![allow(missing_docs)]

    use alloc::boxed::Box;
    use alloc::rc::Rc;
    use alloc::string::String;
    use alloc::vec;
    use alloc::vec::Vec;
    use core::cell::RefCell;
    use core::future::{poll_fn, Future};
    use core::pin::Pin;
    use core::task::Poll;

    use barracuda_rpc::{
        JsonRef, JsonRpcSchema, JsonSchema, JsonWriter, RpcAddress, RpcFrame, RpcLaneStorage,
        RpcMethod, RpcRegistry, Unary,
    };
    use futures_lite::future::block_on;
    use serde_json::{json, Value};

    use super::{execute_steps, validate_definition, WorkflowRuntime};
    use crate::ingress::{EventInput, EventInputPool, InternalEmit};
    use crate::{
        Event, EventEmitter, Rule, WorkflowControlRejection, WorkflowDefinition,
        WorkflowExecutionError, WorkflowId, WorkflowStep,
    };

    const EMPTY_SCHEMA: JsonSchema =
        JsonSchema::new(r#"{"type":"object","properties":{},"additionalProperties":false}"#);
    const TOKEN_SCHEMA: JsonSchema = JsonSchema::new(
        r#"{"type":"object","properties":{"token":{"type":"integer"}},"required":["token"],"additionalProperties":false}"#,
    );
    const DELIVERY_SCHEMA: JsonSchema = JsonSchema::new(
        r#"{"type":"object","properties":{"token":{"type":"integer"},"extra":{"type":"integer"}},"required":["token","extra"],"additionalProperties":false}"#,
    );
    const MODE_SCHEMA: JsonSchema = JsonSchema::new(
        r#"{"type":"object","properties":{"mode":{"type":"string"}},"required":["mode"],"additionalProperties":false}"#,
    );

    struct Produce;

    impl JsonRpcSchema for Produce {
        const ADDRESS: &'static str = "workflow.produce";
        const REQUEST_SCHEMA: JsonSchema = EMPTY_SCHEMA;
        const RESPONSE_SCHEMA: JsonSchema = TOKEN_SCHEMA;
        const MAX_REQUEST_BYTES: usize = 64;
        const MAX_RESPONSE_BYTES: usize = 64;
    }

    struct DirectSink;

    impl JsonRpcSchema for DirectSink {
        const ADDRESS: &'static str = "workflow.direct-sink";
        const REQUEST_SCHEMA: JsonSchema = TOKEN_SCHEMA;
        const RESPONSE_SCHEMA: JsonSchema = EMPTY_SCHEMA;
        const MAX_REQUEST_BYTES: usize = 64;
        const MAX_RESPONSE_BYTES: usize = 2;
    }

    struct MappingSink;

    impl JsonRpcSchema for MappingSink {
        const ADDRESS: &'static str = "workflow.mapping-sink";
        const REQUEST_SCHEMA: JsonSchema = DELIVERY_SCHEMA;
        const RESPONSE_SCHEMA: JsonSchema = EMPTY_SCHEMA;
        const MAX_REQUEST_BYTES: usize = 64;
        const MAX_RESPONSE_BYTES: usize = 2;
    }

    struct LiteralSink;

    impl JsonRpcSchema for LiteralSink {
        const ADDRESS: &'static str = "workflow.literal-sink";
        const REQUEST_SCHEMA: JsonSchema = MODE_SCHEMA;
        const RESPONSE_SCHEMA: JsonSchema = EMPTY_SCHEMA;
        const MAX_REQUEST_BYTES: usize = 64;
        const MAX_RESPONSE_BYTES: usize = 2;
    }

    struct IncompatibleSink;

    impl JsonRpcSchema for IncompatibleSink {
        const ADDRESS: &'static str = "workflow.incompatible-sink";
        const REQUEST_SCHEMA: JsonSchema = MODE_SCHEMA;
        const RESPONSE_SCHEMA: JsonSchema = EMPTY_SCHEMA;
        const MAX_REQUEST_BYTES: usize = 64;
        const MAX_RESPONSE_BYTES: usize = 2;
    }

    struct NativeSink;

    impl RpcMethod for NativeSink {
        const ADDRESS: &'static str = "workflow.native-sink";
        type Request = [u8; 1];
        type Response = [u8; 1];
        type Error = [u8; 1];
        type Input = Unary;
        type Output = Unary;
    }

    struct JsonEvent;

    impl Event for JsonEvent {
        const ID: &'static str = "workflow.event";
    }

    fn address(value: &str) -> RpcAddress {
        RpcAddress::try_from(value).expect("valid test address")
    }

    fn definition(id: &str, steps: Vec<WorkflowStep>) -> WorkflowDefinition {
        WorkflowDefinition::new(
            WorkflowId::try_from(id).expect("valid Workflow ID"),
            Rule::try_from("workflow.event").expect("valid Rule"),
            steps,
        )
        .expect("valid Workflow definition")
    }

    fn event_input(json: &str) -> EventInput {
        Rc::new(EventInputPool::new(1, 256))
            .store(json)
            .expect("store test Event input")
    }

    fn register_producer<const N: usize, const M: usize, const Q: usize>(
        registry: &RpcRegistry<N, M, Q>,
        response: &'static str,
    ) {
        registry
            .register_json::<Produce, _>(
                "*",
                move |_context, _request: JsonRef, writer: JsonWriter| async move {
                    writer.write(response).await
                },
            )
            .expect("register producer");
    }

    #[test]
    fn direct_link_passes_the_previous_json_document_unchanged() {
        let lanes = Box::leak(Box::new(RpcLaneStorage::<2, 128, 2>::new()));
        let registry = RpcRegistry::new(lanes);
        register_producer(&registry, r#"{"token":41}"#);
        let seen = Rc::new(RefCell::new(None));
        let handler_seen = Rc::clone(&seen);
        registry
            .register_json::<DirectSink, _>(
                "*",
                move |_context, request: JsonRef, writer: JsonWriter| {
                    let seen = Rc::clone(&handler_seen);
                    async move {
                        seen.replace(Some(String::from(request.as_str()?)));
                        writer.write("{}").await
                    }
                },
            )
            .expect("register direct sink");
        let client = registry.client();
        let workflow = definition(
            "direct",
            vec![
                WorkflowStep::new(address(Produce::ADDRESS), None),
                WorkflowStep::new(address(DirectSink::ADDRESS), None),
            ],
        );

        validate_definition(&client, &workflow).expect("compatible JSON link");
        let event = event_input("{}");
        block_on(execute_steps(&workflow, &event, client)).expect("execute direct link");

        assert_eq!(seen.take().as_deref(), Some(r#"{"token":41}"#));
    }

    #[test]
    fn event_input_selector_builds_the_first_json_rpc_request() {
        const FRAME_SIZE: usize = 128;

        let lanes = Box::leak(Box::new(RpcLaneStorage::<4, FRAME_SIZE, 4>::new()));
        let registry = RpcRegistry::new(lanes);
        let seen = Rc::new(RefCell::new(None));
        let handler_seen = Rc::clone(&seen);
        registry
            .register_json::<MappingSink, _>(
                "*",
                move |_context, request: JsonRef, writer: JsonWriter| {
                    let seen = Rc::clone(&handler_seen);
                    async move {
                        seen.replace(Some(request.deserialize::<Value>()?));
                        writer.write("{}").await
                    }
                },
            )
            .expect("register mapping sink");
        let mut runtime = WorkflowRuntime::new();
        runtime
            .control()
            .load(definition(
                "event-json",
                vec![WorkflowStep::new(
                    address(MappingSink::ADDRESS),
                    Some(json!({
                        "token": "$event.input.token",
                        "extra": 5
                    })),
                )],
            ))
            .expect("load Workflow");
        registry
            .register_json::<InternalEmit<FRAME_SIZE>, _>(
                "system",
                runtime.ingress_handler::<4, FRAME_SIZE>(),
            )
            .expect("register Event ingress");
        runtime.start(registry.client());
        let emitter = EventEmitter::<FRAME_SIZE>::new(registry.client());
        let mut emit = Box::pin(emitter.emit::<JsonEvent>(r#"{"token":41}"#));
        let mut emit_complete = false;

        block_on(poll_fn(|context| {
            assert!(Pin::new(&mut runtime).poll(context).is_pending());
            if !emit_complete {
                if let Poll::Ready(result) = emit.as_mut().poll(context) {
                    result.expect("emit JSON Event");
                    emit_complete = true;
                }
            }
            if seen.borrow().is_some() {
                Poll::Ready(())
            } else {
                context.waker().wake_by_ref();
                Poll::Pending
            }
        }));

        assert_eq!(seen.take(), Some(json!({"token":41,"extra":5})));
        assert_eq!(runtime.view().info().completed_count, 1);
    }

    #[test]
    fn one_lane_can_emit_and_execute_a_matched_workflow() {
        const FRAME_SIZE: usize = 128;

        let lanes = Box::leak(Box::new(RpcLaneStorage::<1, FRAME_SIZE, 1>::new()));
        let registry = RpcRegistry::new(lanes);
        let seen = Rc::new(RefCell::new(None));
        let handler_seen = Rc::clone(&seen);
        registry
            .register_json::<MappingSink, _>(
                "*",
                move |_context, request: JsonRef, writer: JsonWriter| {
                    let seen = Rc::clone(&handler_seen);
                    async move {
                        seen.replace(Some(request.deserialize::<Value>()?));
                        writer.write("{}").await
                    }
                },
            )
            .expect("register mapping sink");
        let mut runtime = WorkflowRuntime::new();
        runtime
            .control()
            .load(definition(
                "single-lane",
                vec![WorkflowStep::new(
                    address(MappingSink::ADDRESS),
                    Some(json!({"token":"$event.input.token","extra":5})),
                )],
            ))
            .expect("load Workflow");
        registry
            .register_json::<InternalEmit<FRAME_SIZE>, _>(
                "system",
                runtime.ingress_handler::<1, FRAME_SIZE>(),
            )
            .expect("register Event ingress");
        runtime.start(registry.client());
        let emitter = EventEmitter::<FRAME_SIZE>::new(registry.client());
        let mut emit = Box::pin(emitter.emit::<JsonEvent>(r#"{"token":41}"#));
        let mut emit_complete = false;

        block_on(poll_fn(|context| {
            assert!(Pin::new(&mut runtime).poll(context).is_pending());
            if !emit_complete {
                if let Poll::Ready(result) = emit.as_mut().poll(context) {
                    result.expect("emit JSON Event");
                    emit_complete = true;
                }
            }
            if seen.borrow().is_some() {
                Poll::Ready(())
            } else {
                context.waker().wake_by_ref();
                Poll::Pending
            }
        }));

        assert_eq!(seen.take(), Some(json!({"token":41,"extra":5})));
        assert_eq!(runtime.view().info().completed_count, 1);
    }

    #[test]
    fn literal_and_mapping_links_build_json_requests() {
        let lanes = Box::leak(Box::new(RpcLaneStorage::<3, 128, 3>::new()));
        let registry = RpcRegistry::new(lanes);
        register_producer(&registry, r#"{"token":41}"#);
        let mapped = Rc::new(RefCell::new(None));
        let handler_mapped = Rc::clone(&mapped);
        registry
            .register_json::<MappingSink, _>(
                "*",
                move |_context, request: JsonRef, writer: JsonWriter| {
                    let mapped = Rc::clone(&handler_mapped);
                    async move {
                        mapped.replace(Some(request.deserialize::<Value>()?));
                        writer.write("{}").await
                    }
                },
            )
            .expect("register mapping sink");
        let literal = Rc::new(RefCell::new(None));
        let handler_literal = Rc::clone(&literal);
        registry
            .register_json::<LiteralSink, _>(
                "*",
                move |_context, request: JsonRef, writer: JsonWriter| {
                    let literal = Rc::clone(&handler_literal);
                    async move {
                        literal.replace(Some(request.deserialize::<Value>()?));
                        writer.write("{}").await
                    }
                },
            )
            .expect("register literal sink");
        let client = registry.client();
        let mapping = definition(
            "mapping",
            vec![
                WorkflowStep::new(address(Produce::ADDRESS), None),
                WorkflowStep::new(
                    address(MappingSink::ADDRESS),
                    Some(json!({
                        "token":"$previous.output.token",
                        "extra":"$event.input.extra"
                    })),
                ),
            ],
        );
        let literal_workflow = definition(
            "literal",
            vec![
                WorkflowStep::new(address(Produce::ADDRESS), None),
                WorkflowStep::new(address(LiteralSink::ADDRESS), Some(json!({"mode":"quiet"}))),
            ],
        );

        validate_definition(&client, &mapping).expect("valid mapping");
        let mapping_event = event_input(r#"{"extra":5}"#);
        block_on(execute_steps(&mapping, &mapping_event, client.clone())).expect("execute mapping");
        validate_definition(&client, &literal_workflow).expect("valid literal");
        let literal_event = event_input("{}");
        block_on(execute_steps(&literal_workflow, &literal_event, client))
            .expect("execute literal");

        assert_eq!(mapped.take(), Some(json!({"token":41,"extra":5})));
        assert_eq!(literal.take(), Some(json!({"mode":"quiet"})));
    }

    #[test]
    fn validation_requires_json_endpoints_and_compatible_links() {
        let lanes = Box::leak(Box::new(RpcLaneStorage::<3, 128, 3>::new()));
        let registry = RpcRegistry::new(lanes);
        register_producer(&registry, r#"{"token":41}"#);
        registry
            .register_json::<IncompatibleSink, _>(
                "*",
                |_context, _request: JsonRef, writer: JsonWriter| async move {
                    writer.write("{}").await
                },
            )
            .expect("register incompatible sink");
        registry
            .register::<NativeSink, _>("*", |_context, request: RpcFrame<[u8; 1]>| async move {
                Ok(Ok(*request.view()?))
            })
            .expect("register native sink");
        let client = registry.client();
        let incompatible = definition(
            "incompatible",
            vec![
                WorkflowStep::new(address(Produce::ADDRESS), None),
                WorkflowStep::new(address(IncompatibleSink::ADDRESS), None),
            ],
        );
        let native = definition(
            "native",
            vec![WorkflowStep::new(address(NativeSink::ADDRESS), None)],
        );

        assert_eq!(
            validate_definition(&client, &incompatible),
            Err(WorkflowControlRejection::InvalidLink)
        );
        assert_eq!(
            validate_definition(&client, &native),
            Err(WorkflowControlRejection::UnknownMethod)
        );
    }

    #[test]
    fn mapping_fails_when_the_actual_response_omits_a_referenced_field() {
        let lanes = Box::leak(Box::new(RpcLaneStorage::<2, 128, 2>::new()));
        let registry = RpcRegistry::new(lanes);
        register_producer(&registry, "{}");
        registry
            .register_json::<MappingSink, _>(
                "*",
                |_context, _request: JsonRef, writer: JsonWriter| async move {
                    writer.write("{}").await
                },
            )
            .expect("register mapping sink");
        let client = registry.client();
        let workflow = definition(
            "missing-field",
            vec![
                WorkflowStep::new(address(Produce::ADDRESS), None),
                WorkflowStep::new(
                    address(MappingSink::ADDRESS),
                    Some(json!({"token":"$previous.output.token","extra":5})),
                ),
            ],
        );

        let event = event_input("{}");
        let error = block_on(execute_steps(&workflow, &event, client))
            .expect_err("missing referenced field");

        assert!(matches!(
            error,
            WorkflowExecutionError::MissingOutputField { step: 0, field } if field == "token"
        ));
    }

    #[test]
    fn first_step_reports_a_missing_event_input_field() {
        let lanes = Box::leak(Box::new(RpcLaneStorage::<1, 128, 1>::new()));
        let registry = RpcRegistry::new(lanes);
        registry
            .register_json::<MappingSink, _>(
                "*",
                |_context, _request: JsonRef, writer: JsonWriter| async move {
                    writer.write("{}").await
                },
            )
            .expect("register mapping sink");
        let client = registry.client();
        let workflow = definition(
            "missing-event-field",
            vec![WorkflowStep::new(
                address(MappingSink::ADDRESS),
                Some(json!({
                    "token":"$event.input.token",
                    "extra":5
                })),
            )],
        );

        let event = event_input("{}");
        let error = block_on(execute_steps(&workflow, &event, client))
            .expect_err("missing Event input field");

        assert!(matches!(
            error,
            WorkflowExecutionError::MissingEventInputField { field } if field == "token"
        ));
    }

    #[test]
    fn first_step_cannot_select_a_previous_output() {
        let lanes = Box::leak(Box::new(RpcLaneStorage::<1, 128, 1>::new()));
        let registry = RpcRegistry::new(lanes);
        registry
            .register_json::<MappingSink, _>(
                "*",
                |_context, _request: JsonRef, writer: JsonWriter| async move {
                    writer.write("{}").await
                },
            )
            .expect("register mapping sink");
        let workflow = definition(
            "missing-previous",
            vec![WorkflowStep::new(
                address(MappingSink::ADDRESS),
                Some(json!({
                    "token":"$previous.output.token",
                    "extra":5
                })),
            )],
        );

        assert_eq!(
            validate_definition(&registry.client(), &workflow),
            Err(WorkflowControlRejection::InvalidLink)
        );
    }
}
