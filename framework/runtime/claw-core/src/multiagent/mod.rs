//! Optional per-Session multiagent domain component.
//!
//! [`Multiagent`] owns graph policy, tool commands, and the inspection read
//! model. SessionActor remains the sole owner of Agent slots and executes the
//! physical operations requested by this component.
//!
//! Multiagent never owns an Agent, AgentSlot, AgentManager, Scheduler, Session
//! identifier, or persistence policy. Its tools submit semantic commands
//! through a private bridge; SessionActor polls those commands without holding
//! a bridge lock across Agent work.

mod model;
mod plugin;
mod policy;
mod state;
mod tool_port;
mod tools;

pub(crate) use self::model::{
    MultiagentSnapshot, SubagentResult, SubagentSnapshot, SubagentStatus, SubagentTimeout,
    TranscriptText,
};
pub(crate) use self::plugin::Multiagent;
pub(crate) use self::tool_port::{MultiagentAction, SpawnCommand};
