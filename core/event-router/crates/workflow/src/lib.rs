//! Workflow definitions, Event ingress, and Event ID rule matching.
//!
//! Workflow Runtime implementation used by Event Router.

#![no_std]

extern crate alloc;

mod control;
mod definition;
mod event;
mod ingress;
mod link;
mod rule;
mod runtime;

pub use definition::{
    WorkflowDefinition, WorkflowDefinitionError, WorkflowId, WorkflowIdError, WorkflowLoadError,
    WorkflowStep, WorkflowUnloadError,
};
pub use link::LinkError;
pub use event::{
    EmitError, EmitRejection, Event, EventEmitter, EventId, EventIdError, EventInputMode,
};
pub use rule::{Rule, RuleError};
pub use runtime::{
    validate_definition, WorkflowExecutionError, WorkflowFailure, WorkflowInfo,
};

/// Runtime integration surface used by Event Router's Component adapter.
pub mod integration {
    pub use crate::control::{
        WorkflowJsonFrame, WorkflowJsonRequest, WorkflowLoad, WorkflowUnload,
    };
    pub use crate::ingress::{EmitErrorFrame, InternalEmit, InternalEmitFrame};
    pub use crate::runtime::{WorkflowRuntime, WorkflowRuntimeControl, WorkflowRuntimeView};
}
pub use control::{WorkflowClient, WorkflowControlError, WorkflowControlRejection};
