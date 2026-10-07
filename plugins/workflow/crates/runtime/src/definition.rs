//! Workflow identities and immutable Workflow definitions.

use alloc::string::String;
use alloc::vec::Vec;
use core::fmt;

use getset::Getters;
use serde_json::Value;

use super::link::{classify, LinkError, LinkKind, SourceSelector};
use super::{Rule, Topic, WorkflowActionAddress};

/// Stable identifier of one Workflow definition.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct WorkflowId(String);

impl WorkflowId {
    /// Returns this Workflow ID as a string slice.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<&str> for WorkflowId {
    type Error = WorkflowIdError;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        Self::try_from(String::from(value))
    }
}

impl TryFrom<String> for WorkflowId {
    type Error = WorkflowIdError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        if value.is_empty() {
            return Err(WorkflowIdError::Empty);
        }
        for (index, character) in value.char_indices() {
            let valid = character.is_ascii_alphanumeric() || matches!(character, '_' | '-' | '.');
            if !valid {
                return Err(WorkflowIdError::InvalidCharacter { index, character });
            }
        }
        Ok(Self(value))
    }
}

impl fmt::Display for WorkflowId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// Failure returned while parsing a [`WorkflowId`].
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum WorkflowIdError {
    /// Workflow IDs cannot be empty.
    #[error("Workflow ID cannot be empty")]
    Empty,
    /// Workflow IDs contain only ASCII letters, digits, `_`, `-`, and `.`.
    #[error("invalid Workflow ID character {character:?} at byte {index}")]
    InvalidCharacter {
        /// Byte offset of the invalid character.
        index: usize,
        /// Invalid character found in the input.
        character: char,
    },
}

/// One ordered Action step in a Workflow, with optional link arguments.
///
/// Building a [`WorkflowDefinition`] moves the arguments into the step's
/// classified link, so they are stored once.
#[derive(Clone, Debug, Getters, PartialEq)]
pub struct WorkflowStep {
    /// Action address invoked by this step.
    #[getset(get = "pub")]
    address: WorkflowActionAddress,
    arguments: Option<Value>,
}

impl WorkflowStep {
    /// Creates a step with an address and optional link arguments.
    #[must_use]
    pub fn new(address: WorkflowActionAddress, arguments: Option<Value>) -> Self {
        Self { address, arguments }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct WorkflowCondition {
    pub(crate) selector: SourceSelector,
    pub(crate) field: Option<String>,
    pub(crate) comparison: WorkflowComparison,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum WorkflowComparison {
    Equals(Value),
    NotEquals(Value),
}

impl WorkflowComparison {
    pub(crate) fn matches(&self, actual: &Value) -> bool {
        match self {
            Self::Equals(expected) => actual == expected,
            Self::NotEquals(expected) => actual != expected,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct WorkflowBranch {
    pub(crate) condition: WorkflowCondition,
    pub(crate) then_operations: Vec<WorkflowOperation>,
    pub(crate) else_operations: Vec<WorkflowOperation>,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum WorkflowOperation {
    Call(usize),
    Return,
    Branch(WorkflowBranch),
}

/// One immutable Workflow definition loaded into the Runtime.
#[derive(Clone, Debug, Getters, PartialEq)]
pub struct WorkflowDefinition {
    /// Stable Workflow identity.
    #[getset(get = "pub")]
    id: WorkflowId,
    /// Event rule that starts this Workflow.
    #[getset(get = "pub")]
    event: Rule,
    topic: Option<Topic>,
    steps: Vec<WorkflowStep>,
    links: Vec<LinkKind>,
    returns: bool,
    operations: Vec<WorkflowOperation>,
}

impl WorkflowDefinition {
    /// Creates a Workflow with at least one Action step.
    pub fn new(
        id: WorkflowId,
        event: Rule,
        steps: Vec<WorkflowStep>,
    ) -> Result<Self, WorkflowDefinitionError> {
        Self::from_parts(id, event, None, steps, false)
    }

    fn from_parts(
        id: WorkflowId,
        event: Rule,
        topic: Option<Topic>,
        steps: Vec<WorkflowStep>,
        returns: bool,
    ) -> Result<Self, WorkflowDefinitionError> {
        if steps.is_empty() && !returns {
            return Err(WorkflowDefinitionError::EmptySteps);
        }
        let (steps, links) = classify_steps(steps)?;
        let operations = linear_operations(steps.len(), returns);
        Ok(Self {
            id,
            event,
            topic,
            steps,
            links,
            returns,
            operations,
        })
    }

    pub(crate) fn with_operations(
        id: WorkflowId,
        event: Rule,
        topic: Option<Topic>,
        steps: Vec<WorkflowStep>,
        operations: Vec<WorkflowOperation>,
        returns: bool,
    ) -> Result<Self, WorkflowDefinitionError> {
        if operations.is_empty() {
            return Err(WorkflowDefinitionError::EmptySteps);
        }
        let (steps, links) = classify_steps(steps)?;
        let mut operations = operations;
        operations.shrink_to_fit();
        Ok(Self {
            id,
            event,
            topic,
            steps,
            links,
            returns,
            operations,
        })
    }

    /// Returns the optional Event topic rule.
    #[must_use]
    pub fn topic(&self) -> Option<&Topic> {
        self.topic.as_ref()
    }

    /// Returns every declared Action call in document order.
    #[must_use]
    pub fn steps(&self) -> &[WorkflowStep] {
        &self.steps
    }

    /// Returns whether this Workflow contains an explicit return.
    #[must_use]
    pub const fn returns(&self) -> bool {
        self.returns
    }

    /// Returns whether this Workflow contains a conditional branch.
    #[must_use]
    pub fn has_branch(&self) -> bool {
        operations_have_branch(&self.operations)
    }

    pub(crate) fn links(&self) -> &[LinkKind] {
        &self.links
    }
    pub(crate) fn operations(&self) -> &[WorkflowOperation] {
        &self.operations
    }
}

fn linear_operations(step_count: usize, returns: bool) -> Vec<WorkflowOperation> {
    let mut operations = (0..step_count)
        .map(WorkflowOperation::Call)
        .collect::<Vec<_>>();
    if returns {
        operations.push(WorkflowOperation::Return);
    }
    operations
}

fn operations_have_branch(operations: &[WorkflowOperation]) -> bool {
    operations.iter().any(|operation| match operation {
        WorkflowOperation::Call(_) | WorkflowOperation::Return => false,
        WorkflowOperation::Branch(_) => true,
    })
}

/// Moves each step's arguments into its classified link.
fn classify_steps(
    mut steps: Vec<WorkflowStep>,
) -> Result<(Vec<WorkflowStep>, Vec<LinkKind>), WorkflowDefinitionError> {
    let links = steps
        .iter_mut()
        .map(|step| {
            classify(step.arguments.take()).map_err(WorkflowDefinitionError::InvalidReference)
        })
        .collect::<Result<Vec<_>, _>>()?;
    steps.shrink_to_fit();
    Ok((steps, links))
}

/// Failure while constructing a [`WorkflowDefinition`].
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum WorkflowDefinitionError {
    /// Every Workflow needs at least one executable operation.
    #[error("Workflow must contain at least one Action step")]
    EmptySteps,
    /// A step's `$` reference arguments were malformed.
    #[error("invalid Workflow step reference: {0}")]
    InvalidReference(#[source] LinkError),
}

/// Failure while loading a Workflow definition.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum WorkflowLoadError {
    /// Another loaded Workflow already owns this ID.
    #[error("Workflow is already loaded: {0}")]
    DuplicateId(WorkflowId),
}

/// Failure while unloading a Workflow definition.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum WorkflowUnloadError {
    /// No loaded Workflow owns this ID.
    #[error("Workflow is not loaded: {0}")]
    NotFound(WorkflowId),
}
