//! Optional per-Session multiagent domain component.
//!
//! [`Multiagent`] owns graph policy, tool commands, and the inspection read
//! model. Its physical host owns Agent slots and executes the operations
//! requested by this component.
//!
//! Multiagent never owns an Agent, AgentSlot, AgentManager, Session identifier,
//! or persistence policy. Its tools submit semantic commands
//! through a private bridge; the host polls those commands without holding a
//! bridge lock across Agent work.

mod component;
mod effect;
mod host;
mod model;
mod orchestrator;
mod policy;
mod state;
mod tool_port;
mod tools;

pub(crate) use self::component::Multiagent;
pub(crate) use self::effect::{
    DispatchOutcome, InterruptOutcome, MultiagentEffect, MultiagentPhysicalError,
};
pub(crate) use self::host::MultiagentHost;
pub(crate) use self::model::SubagentTimeout;
