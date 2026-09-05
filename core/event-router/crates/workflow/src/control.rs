//! Lane-native JSON protocol for Workflow control RPCs.

use alloc::string::String;
use alloc::vec::Vec;

use barracuda_rpc::{
    json_schema, JsonObjectPayload, JsonPayload, JsonRpcSchema, JsonSchema, JsonWriter, RpcAddress,
    RpcClient, RpcError,
};
use serde::Deserialize;
use serde_json::Value;

use crate::definition::{
    WorkflowBranch, WorkflowCondition, WorkflowDefinitionError, WorkflowOperation,
};
use crate::link::parse_reference;
use crate::{Rule, Topic, WorkflowDefinition, WorkflowId, WorkflowStep};

const CONTROL_RESPONSE_MAX_BYTES: usize = 40;

/// Receiver-side rejection returned by a Workflow control RPC.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum WorkflowControlRejection {
    /// The request was not a valid Workflow JSON document.
    InvalidJson,
    /// The JSON contained an invalid Workflow ID.
    InvalidWorkflowId,
    /// The JSON contained an invalid Event rule.
    InvalidRule,
    /// The JSON contained an invalid RPC address.
    InvalidRpcAddress,
    /// The Workflow did not contain an RPC step.
    EmptySteps,
    /// Another loaded Workflow already owns the requested ID.
    DuplicateId,
    /// No loaded Workflow owns the requested ID.
    NotFound,
    /// The persistence operation failed.
    Persistence,
    /// A step's link arguments were malformed.
    InvalidArguments,
    /// A step addressed a method that is not registered.
    UnknownMethod,
    /// A step's link violated a validation rule.
    InvalidLink,
    /// The JSON contained an invalid Event topic.
    InvalidTopic,
    /// The Workflow control-flow structure was invalid.
    InvalidControlFlow,
}

impl WorkflowControlRejection {
    const fn code(self) -> &'static str {
        match self {
            Self::InvalidJson => "invalid_json",
            Self::InvalidWorkflowId => "invalid_workflow_id",
            Self::InvalidRule => "invalid_rule",
            Self::InvalidRpcAddress => "invalid_rpc_address",
            Self::EmptySteps => "empty_steps",
            Self::DuplicateId => "duplicate_id",
            Self::NotFound => "not_found",
            Self::Persistence => "persistence",
            Self::InvalidArguments => "invalid_arguments",
            Self::UnknownMethod => "unknown_method",
            Self::InvalidLink => "invalid_link",
            Self::InvalidTopic => "invalid_topic",
            Self::InvalidControlFlow => "invalid_control_flow",
        }
    }
}

/// Failure returned by [`WorkflowClient`].
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum WorkflowControlError {
    /// RPC transport or runtime failure.
    #[error(transparent)]
    Rpc(#[from] RpcError),
    /// Workflow Runtime rejected the control request.
    #[error("Workflow control request was rejected: {0:?}")]
    Rejected(WorkflowControlRejection),
}

/// JSON RPC that validates and durably loads one Workflow document.
pub struct WorkflowLoad<const M: usize>;

impl<const M: usize> JsonRpcSchema for WorkflowLoad<M> {
    const ADDRESS: &'static str = "workflow.load";
    const REQUEST_SCHEMA: JsonSchema = json_schema!("workflow_load", request);
    const RESPONSE_SCHEMA: JsonSchema = json_schema!("workflow_load", response);
    const MAX_REQUEST_BYTES: usize = M;
    const MAX_RESPONSE_BYTES: usize = CONTROL_RESPONSE_MAX_BYTES;
}

/// JSON RPC that durably unloads one Workflow selected by ID.
pub struct WorkflowUnload<const M: usize>;

impl<const M: usize> JsonRpcSchema for WorkflowUnload<M> {
    const ADDRESS: &'static str = "workflow.unload";
    const REQUEST_SCHEMA: JsonSchema = json_schema!("workflow_unload", request);
    const RESPONSE_SCHEMA: JsonSchema = json_schema!("workflow_unload", response);
    const MAX_REQUEST_BYTES: usize = M;
    const MAX_RESPONSE_BYTES: usize = CONTROL_RESPONSE_MAX_BYTES;
}

/// Client for Workflow Runtime's durable control RPCs.
#[derive(Clone)]
pub struct WorkflowClient<const M: usize> {
    rpc: RpcClient,
}

impl<const M: usize> WorkflowClient<M> {
    /// Wraps an existing RPC client.
    #[must_use]
    pub const fn new(rpc: RpcClient) -> Self {
        Self { rpc }
    }

    /// Loads one complete Workflow JSON document.
    ///
    /// # Errors
    ///
    /// Returns a transport error or the Runtime's validation, duplicate-ID, or
    /// persistence rejection.
    pub async fn load(&self, json: &str) -> Result<(), WorkflowControlError> {
        self.call::<WorkflowLoad<M>, _>(json).await
    }

    /// Unloads one Workflow by ID.
    ///
    /// # Errors
    ///
    /// Returns a transport error, a not-found rejection, or a persistence
    /// rejection.
    pub async fn unload(&self, workflow_id: &WorkflowId) -> Result<(), WorkflowControlError> {
        let fields = |writer: &mut barracuda_rpc::JsonObjectWriter<'_>| {
            writer.string_field("id", workflow_id.as_str())
        };
        self.call::<WorkflowUnload<M>, _>(&JsonObjectPayload::new(&fields))
            .await
    }

    async fn call<Method, J>(&self, request: &J) -> Result<(), WorkflowControlError>
    where
        Method: JsonRpcSchema,
        J: JsonPayload + ?Sized,
    {
        let address = RpcAddress::try_from(Method::ADDRESS).map_err(RpcError::from)?;
        let response = self.rpc.call_json(&address, request)?.await?;
        let response: WorkflowControlResponse = response.deserialize()?;
        match response.error {
            Some(rejection) => Err(WorkflowControlError::Rejected(rejection)),
            None => Ok(()),
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WorkflowControlResponse {
    #[serde(default)]
    error: Option<WorkflowControlRejection>,
}

/// Writes `{}` for success or a stable JSON rejection code for failure.
pub async fn write_control_response(
    response: JsonWriter,
    result: Result<(), WorkflowControlRejection>,
) -> Result<(), RpcError> {
    match result {
        Ok(()) => response.write("{}").await,
        Err(rejection) => {
            let fields = |writer: &mut barracuda_rpc::JsonObjectWriter<'_>| {
                writer.string_field("error", rejection.code())
            };
            response.write(&JsonObjectPayload::new(&fields)).await
        }
    }
}

/// Parses one Workflow definition from its original JSON document.
pub fn parse_definition(json: &str) -> Result<WorkflowDefinition, WorkflowControlRejection> {
    let document: WorkflowDocument =
        serde_json::from_str(json).map_err(|_error| WorkflowControlRejection::InvalidJson)?;
    document.try_into()
}

/// Parses one Workflow ID request document.
pub fn parse_workflow_id(json: &str) -> Result<WorkflowId, WorkflowControlRejection> {
    let document: WorkflowIdDocument =
        serde_json::from_str(json).map_err(|_error| WorkflowControlRejection::InvalidJson)?;
    WorkflowId::try_from(document.id).map_err(|_error| WorkflowControlRejection::InvalidWorkflowId)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WorkflowDocument {
    id: String,
    #[serde(rename = "match")]
    matcher: WorkflowMatchDocument,
    steps: Vec<WorkflowStepDocument>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WorkflowMatchDocument {
    event: String,
    #[serde(default)]
    topic: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WorkflowCallDocument {
    call: String,
    #[serde(default)]
    arguments: Option<Value>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WorkflowReturnDocument {
    #[serde(rename = "return")]
    _value: EmptyReturnDocument,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EmptyReturnDocument {}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WorkflowBranchDocument {
    #[serde(rename = "if")]
    condition: String,
    then: Vec<WorkflowStepDocument>,
    #[serde(rename = "else")]
    otherwise: Vec<WorkflowStepDocument>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum WorkflowStepDocument {
    Call(WorkflowCallDocument),
    Return(WorkflowReturnDocument),
    Branch(WorkflowBranchDocument),
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WorkflowIdDocument {
    id: String,
}

impl TryFrom<WorkflowDocument> for WorkflowDefinition {
    type Error = WorkflowControlRejection;

    fn try_from(document: WorkflowDocument) -> Result<Self, Self::Error> {
        let id = WorkflowId::try_from(document.id)
            .map_err(|_error| WorkflowControlRejection::InvalidWorkflowId)?;
        let event = Rule::try_from(document.matcher.event)
            .map_err(|_error| WorkflowControlRejection::InvalidRule)?;
        let topic = document
            .matcher
            .topic
            .map(Topic::try_from)
            .transpose()
            .map_err(|_error| WorkflowControlRejection::InvalidTopic)?;
        let mut steps = Vec::new();
        let (operations, returns) = parse_operations(document.steps, &mut steps)?;
        let definition =
            WorkflowDefinition::with_operations(id, event, topic, steps, operations, returns);
        definition.map_err(|error| match error {
            WorkflowDefinitionError::EmptySteps => WorkflowControlRejection::EmptySteps,
            WorkflowDefinitionError::InvalidReference(_) => {
                WorkflowControlRejection::InvalidArguments
            }
        })
    }
}

fn parse_operations(
    operations: Vec<WorkflowStepDocument>,
    steps: &mut Vec<WorkflowStep>,
) -> Result<(Vec<WorkflowOperation>, bool), WorkflowControlRejection> {
    let mut parsed = Vec::new();
    let mut returns = false;
    let mut operations = operations.into_iter().peekable();
    while let Some(operation) = operations.next() {
        match operation {
            WorkflowStepDocument::Call(step) => {
                let address = RpcAddress::try_from(step.call.as_str())
                    .map_err(|_error| WorkflowControlRejection::InvalidRpcAddress)?;
                let index = steps.len();
                steps.push(WorkflowStep::new(address, step.arguments));
                parsed.push(WorkflowOperation::Call(index));
            }
            WorkflowStepDocument::Return(_) => {
                if operations.peek().is_some() {
                    return Err(WorkflowControlRejection::InvalidControlFlow);
                }
                parsed.push(WorkflowOperation::Return);
                returns = true;
            }
            WorkflowStepDocument::Branch(branch) => {
                let body = branch
                    .condition
                    .strip_prefix('$')
                    .ok_or(WorkflowControlRejection::InvalidControlFlow)?;
                let (selector, field) = parse_reference(body)
                    .map_err(|_error| WorkflowControlRejection::InvalidControlFlow)?;
                let (then_operations, then_returns) = parse_operations(branch.then, steps)?;
                let (else_operations, else_returns) = parse_operations(branch.otherwise, steps)?;
                parsed.push(WorkflowOperation::Branch(WorkflowBranch {
                    condition: WorkflowCondition { selector, field },
                    then_operations,
                    else_operations,
                }));
                returns = returns || then_returns || else_returns;
            }
        }
    }
    Ok((parsed, returns))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]
    #![allow(missing_docs)]

    use alloc::boxed::Box;
    use alloc::vec::Vec;

    use barracuda_rpc::{JsonRpcSchema, JsonWriter, RpcLaneStorage, RpcRegistry};
    use futures_lite::future::block_on;

    use super::{
        parse_definition, WorkflowClient, WorkflowControlError, WorkflowControlRejection,
        WorkflowLoad, WorkflowUnload,
    };

    #[test]
    fn workflow_control_methods_are_lane_native_json() {
        assert_eq!(WorkflowLoad::<256>::ADDRESS, "workflow.load");
        assert_eq!(WorkflowLoad::<256>::MAX_REQUEST_BYTES, 256);
        assert_eq!(WorkflowUnload::<256>::ADDRESS, "workflow.unload");
        assert_eq!(WorkflowUnload::<256>::MAX_REQUEST_BYTES, 256);
    }

    #[test]
    fn workflow_client_reads_json_success_and_rejection_responses() {
        block_on(async {
            let lanes = Box::leak(Box::new(RpcLaneStorage::<1, 128, 1>::new()));
            let registry = RpcRegistry::new(lanes);
            registry
                .register_json::<WorkflowLoad<128>, _>(
                    "system",
                    |_context, _request, response: JsonWriter| async move {
                        response.write("{}").await
                    },
                )
                .expect("register load control");
            registry
                .register_json::<WorkflowUnload<128>, _>(
                    "system",
                    |_context, _request, response: JsonWriter| async move {
                        response.write(r#"{"error":"not_found"}"#).await
                    },
                )
                .expect("register unload control");
            let client = WorkflowClient::<128>::new(registry.client());

            client
                .load(r#"{"id":"a","match":{"event":"a"},"steps":[{"call":"a.b"}]}"#)
                .await
                .expect("read empty success response");
            let id = crate::WorkflowId::try_from("a").expect("valid Workflow ID");
            assert_eq!(
                client.unload(&id).await,
                Err(WorkflowControlError::Rejected(
                    WorkflowControlRejection::NotFound
                ))
            );
        });
    }

    #[test]
    fn workflow_json_uses_the_documented_match_and_step_shape() {
        let definition = parse_definition(
            r#"{
                "id":"gateway-to-agent",
                "match":{"event":"gateway.*"},
                "steps":[{"call":"adapter.gateway"},{"call":"agent.run"}]
            }"#,
        )
        .expect("valid Workflow JSON");

        assert_eq!(definition.id().as_str(), "gateway-to-agent");
        assert_eq!(definition.event().as_str(), "gateway.*");
        assert!(definition.topic().is_none());
        assert_eq!(
            definition
                .steps()
                .iter()
                .map(|step| step.address().as_ref())
                .collect::<Vec<_>>(),
            ["adapter.gateway", "agent.run"]
        );
    }

    #[test]
    fn workflow_json_accepts_an_optional_exact_topic() {
        let definition = parse_definition(
            r#"{
                "id":"morning-alarm",
                "match":{"event":"scheduler.triggered","topic":"morning"},
                "steps":[{"call":"alarm.ring"}]
            }"#,
        )
        .expect("valid Workflow JSON");
        assert_eq!(
            definition.topic().map(crate::Topic::as_str),
            Some("morning")
        );

        let invalid = parse_definition(
            r#"{"id":"bad","match":{"event":"scheduler.triggered","topic":"topic-name-is-over-16"},"steps":[{"call":"alarm.ring"}]}"#,
        );
        assert!(matches!(
            invalid,
            Err(WorkflowControlRejection::InvalidTopic)
        ));

        let wildcard = parse_definition(
            r#"{"id":"wildcard","match":{"event":"scheduler.triggered","topic":"*"},"steps":[{"call":"alarm.ring"}]}"#,
        );
        assert!(matches!(
            wildcard,
            Err(WorkflowControlRejection::InvalidTopic)
        ));
    }

    #[test]
    fn workflow_json_rejects_invalid_json_and_empty_steps() {
        assert!(matches!(
            parse_definition("{"),
            Err(WorkflowControlRejection::InvalidJson)
        ));

        let empty_steps =
            parse_definition(r#"{"id":"empty","match":{"event":"gateway.*"},"steps":[]}"#);
        assert!(matches!(
            empty_steps,
            Err(WorkflowControlRejection::EmptySteps)
        ));
    }

    #[test]
    fn workflow_json_accepts_a_successful_return_terminal() {
        let definition = parse_definition(
            r#"{
                "id":"ignore-event",
                "match":{"event":"gateway.*"},
                "steps":[{"return":{}}]
            }"#,
        )
        .expect("return-only Workflow");

        assert!(definition.steps().is_empty());
        assert!(definition.returns());
    }

    #[test]
    fn workflow_json_rejects_steps_after_return() {
        let result = parse_definition(
            r#"{
                "id":"unreachable",
                "match":{"event":"gateway.*"},
                "steps":[{"return":{}},{"call":"agent.run"}]
            }"#,
        );

        assert_eq!(result, Err(WorkflowControlRejection::InvalidControlFlow));
    }

    #[test]
    fn workflow_json_accepts_dynamic_and_nested_if_else() {
        let definition = parse_definition(
            r#"{
                "id":"forward-output",
                "match":{"event":"session.event"},
                "steps":[
                    {"call":"imessage_bridge.to_gateway"},
                    {
                        "if":"$previous.output.forward",
                        "then":[
                            {
                                "if":"$event.input.enabled",
                                "then":[{"call":"gateway.send_stream"}],
                                "else":[]
                            }
                        ],
                        "else":[]
                    },
                    {"call":"audit.record"}
                ]
            }"#,
        )
        .expect("dynamic branch Workflow");

        assert_eq!(definition.steps().len(), 3);
        assert!(!definition.returns());
        assert!(definition.has_branch());
    }

    #[test]
    fn workflow_json_rejects_invalid_branch_control_flow() {
        for json in [
            r#"{"id":"after-return","match":{"event":"a"},"steps":[{"if":"$event.input.ok","then":[{"return":{}},{"call":"a.b"}],"else":[]}]}"#,
            r#"{"id":"bad-condition","match":{"event":"a"},"steps":[{"if":"event.input.ok","then":[],"else":[]}]}"#,
            r#"{"id":"nested-field","match":{"event":"a"},"steps":[{"if":"$event.input.flags.ok","then":[],"else":[]}]}"#,
        ] {
            assert_eq!(
                parse_definition(json),
                Err(WorkflowControlRejection::InvalidControlFlow)
            );
        }
    }
}
