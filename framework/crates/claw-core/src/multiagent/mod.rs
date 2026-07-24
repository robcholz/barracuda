//! Optional per-Session multiagent domain component.
//!
//! [`Multiagent`] owns graph policy, tool commands, and the inspection read
//! model. SessionActor remains the sole owner of Agent slots and executes the
//! physical operations requested by this component.
//!
//! Multiagent never owns an Agent, AgentSlot, AgentManager, Session identifier,
//! or persistence policy. Its tools submit semantic commands
//! through a private bridge; SessionActor polls those commands without holding
//! a bridge lock across Agent work.

mod component;
mod model;
mod policy;
mod state;
mod tool_port;
mod tools;

pub(crate) use self::component::{
    DispatchOutcome, Multiagent, MultiagentEffect, MultiagentEffectResult, MultiagentPhysicalError,
};
pub(crate) use self::model::SubagentTimeout;
pub(crate) use self::tool_port::SpawnCommand;
