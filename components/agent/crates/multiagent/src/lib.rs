#![no_std]
#![allow(clippy::arc_with_non_send_sync)]

//! Optional per-Session multiagent domain component.
//!
//! [`Multiagent`] owns graph policy, tool commands, and the inspection read
//! model. Its effects describe physical operations without executing them.
//!
//! Multiagent never owns an Agent, AgentSlot, AgentManager, Session identifier,
//! or persistence policy. Its tools submit semantic commands
//! through a private bridge; the integration layer polls those commands
//! without holding a bridge lock across Agent work.

#[macro_use]
extern crate alloc;

mod component;
mod effect;
mod model;
mod policy;
mod state;
mod tool_port;
mod tools;

pub use self::component::Multiagent;
pub use self::effect::{
    DispatchOutcome, EffectId, InterruptOutcome, MultiagentEffect, MultiagentEffectResult,
    MultiagentPhysicalError,
};
pub use self::model::{SubagentSpec, SubagentTimeout};
