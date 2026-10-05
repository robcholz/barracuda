//! Durable Workflow definitions and direct Action execution.

#![no_std]

extern crate alloc;

mod action;
mod control;
mod definition;
mod event;
mod link;
mod rule;
mod runtime;
mod topic;

pub use action::{
    WorkflowActionAddress, WorkflowActionAddressError, WorkflowActionDescriptor,
    WorkflowActionError, WorkflowActionFuture, WorkflowActionHandler, WorkflowActionRegistration,
    WorkflowActionRegistry, WorkflowActionRegistryError, WorkflowActionSchema,
};
pub use control::{parse_definition, WorkflowControlRejection};
pub use definition::{
    WorkflowDefinition, WorkflowDefinitionError, WorkflowId, WorkflowIdError, WorkflowLoadError,
    WorkflowStep, WorkflowUnloadError,
};
pub use event::{EmitError, Event, EventId, EventIdError};
pub use link::LinkError;
pub use rule::{Rule, RuleError};
pub use runtime::{
    validate_definition, WorkflowExecutionError, WorkflowFailure, WorkflowInfo, WorkflowRuntime,
    WorkflowRuntimeControl, WorkflowRuntimeView, EVENT_BACKLOG_LIMIT,
};
pub use topic::{Topic, TopicError, TOPIC_MAX_BYTES};

/// Owned JSON value passed directly between Workflow Actions.
pub type WorkflowValue = serde_json::Value;

/// Dependencies used by exported schema macros.
#[doc(hidden)]
pub mod __private {
    pub use json_validator;
}
