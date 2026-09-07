//! Parsing for the existing Workflow JSON DSL.

use alloc::string::String;
use alloc::vec::Vec;
use serde::Deserialize;
use serde_json::Value;

use crate::definition::{
    WorkflowBranch, WorkflowComparison, WorkflowCondition, WorkflowDefinitionError,
    WorkflowOperation,
};
use crate::link::parse_condition_source;
use crate::{Rule, Topic, WorkflowActionAddress, WorkflowDefinition, WorkflowId, WorkflowStep};

/// Stable rejection returned by Workflow control operations.
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
    /// The JSON contained an invalid Action address.
    InvalidActionAddress,
    /// The Workflow did not contain an Action step.
    EmptySteps,
    /// Another loaded Workflow already owns the requested ID.
    DuplicateId,
    /// No loaded Workflow owns the requested ID.
    NotFound,
    /// The persistence operation failed.
    Persistence,
    /// A step's link arguments were malformed.
    InvalidArguments,
    /// A step addressed an Action that is not registered.
    UnknownAction,
    /// A step's link violated a validation rule.
    InvalidLink,
    /// The JSON contained an invalid Event topic.
    InvalidTopic,
    /// The Workflow control-flow structure was invalid.
    InvalidControlFlow,
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
struct WorkflowEqualsConditionDocument {
    source: String,
    equals: Value,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WorkflowNotEqualsConditionDocument {
    source: String,
    not_equals: Value,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum WorkflowConditionDocument {
    Equals(WorkflowEqualsConditionDocument),
    NotEquals(WorkflowNotEqualsConditionDocument),
}

impl WorkflowConditionDocument {
    fn into_parts(self) -> (String, WorkflowComparison) {
        match self {
            Self::Equals(condition) => (
                condition.source,
                WorkflowComparison::Equals(condition.equals),
            ),
            Self::NotEquals(condition) => (
                condition.source,
                WorkflowComparison::NotEquals(condition.not_equals),
            ),
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WorkflowBranchDocument {
    #[serde(rename = "if")]
    condition: WorkflowConditionDocument,
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
        WorkflowDefinition::with_operations(id, event, topic, steps, operations, returns).map_err(
            |error| match error {
                WorkflowDefinitionError::EmptySteps => WorkflowControlRejection::EmptySteps,
                WorkflowDefinitionError::InvalidReference(_) => {
                    WorkflowControlRejection::InvalidArguments
                }
            },
        )
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
                let address = WorkflowActionAddress::try_from(step.call)
                    .map_err(|_error| WorkflowControlRejection::InvalidActionAddress)?;
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
                let (source, comparison) = branch.condition.into_parts();
                let body = source
                    .strip_prefix('$')
                    .ok_or(WorkflowControlRejection::InvalidControlFlow)?;
                let (selector, field) = parse_condition_source(body)
                    .map_err(|_error| WorkflowControlRejection::InvalidControlFlow)?;
                let (then_operations, then_returns) = parse_operations(branch.then, steps)?;
                let (else_operations, else_returns) = parse_operations(branch.otherwise, steps)?;
                parsed.push(WorkflowOperation::Branch(WorkflowBranch {
                    condition: WorkflowCondition {
                        selector,
                        field,
                        comparison,
                    },
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

    use super::parse_definition;

    #[test]
    fn existing_dsl_parses_without_transport_fields() {
        let workflow = parse_definition(
            r#"{"id":"reply","match":{"event":"gateway.*","topic":"primary"},"steps":[{"call":"agent.run","arguments":{"prompt":"$event.input.message"}},{"if":{"source":"$previous.output.ok","equals":true},"then":[{"call":"gateway.send"},{"return":{}}],"else":[{"return":{}}]}]}"#,
        ).expect("existing Workflow DSL");

        assert_eq!(workflow.id().as_str(), "reply");
        assert_eq!(workflow.steps().len(), 2);
        assert!(workflow.has_branch());
        assert!(workflow.returns());
    }
}
