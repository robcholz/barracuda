//! Workflow identities and immutable Workflow definitions.

use alloc::string::String;
use alloc::vec::Vec;
use core::fmt;

use barracuda_rpc::RpcAddress;
use getset::Getters;

use super::Rule;

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

impl AsRef<str> for WorkflowId {
    fn as_ref(&self) -> &str {
        self.as_str()
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

/// One immutable Workflow definition loaded into the Workflow Runtime.
#[derive(Clone, Debug, Getters, PartialEq, Eq)]
pub struct WorkflowDefinition {
    /// Stable Workflow identity.
    #[getset(get = "pub")]
    id: WorkflowId,
    /// Event ID rule that starts this Workflow.
    #[getset(get = "pub")]
    event: Rule,
    steps: Vec<RpcAddress>,
}

impl WorkflowDefinition {
    /// Creates a Workflow with at least one RPC step.
    ///
    /// # Errors
    ///
    /// Returns [`WorkflowDefinitionError::EmptySteps`] when no ingress RPC is
    /// present.
    pub fn new(
        id: WorkflowId,
        event: Rule,
        steps: Vec<RpcAddress>,
    ) -> Result<Self, WorkflowDefinitionError> {
        if steps.is_empty() {
            return Err(WorkflowDefinitionError::EmptySteps);
        }
        Ok(Self { id, event, steps })
    }

    /// Returns the ordered RPC call flow.
    #[must_use]
    pub fn steps(&self) -> &[RpcAddress] {
        &self.steps
    }
}

/// Failure while constructing a [`WorkflowDefinition`].
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum WorkflowDefinitionError {
    /// Every Workflow needs an ingress RPC.
    #[error("Workflow must contain at least one RPC step")]
    EmptySteps,
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

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]
    #![allow(missing_docs)]

    use alloc::vec;

    use super::{WorkflowDefinition, WorkflowDefinitionError, WorkflowId};
    use crate::Rule;

    fn rule(value: &str) -> Rule {
        Rule::try_from(value).expect("valid test Rule")
    }

    #[test]
    fn workflow_definition_requires_at_least_one_rpc_step() {
        let result = WorkflowDefinition::new(
            WorkflowId::try_from("empty").expect("valid Workflow ID"),
            rule("gateway.*"),
            vec![],
        );

        assert!(matches!(result, Err(WorkflowDefinitionError::EmptySteps)));
    }
}
